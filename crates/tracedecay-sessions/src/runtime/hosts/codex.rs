//! Codex CLI transcript source.
//!
//! Codex appends one JSON object per line to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (sessions archived from the
//! picker move to a flat `~/.codex/archived_sessions/rollout-*.jsonl`). Each
//! line is `{"timestamp": "<iso8601>", "type": "<kind>", "payload": {…}}`. The
//! relevant kinds for conversation text are:
//!
//! * `session_meta` — first line; `payload.cwd`, session `id`. Real rollouts
//!   carry no `model` here (only `model_provider`); the active model is on
//!   `turn_context` lines and can change mid-session.
//! * `event_msg` with `payload.type == "item_completed"` and
//!   `payload.item.type == "UserMessage"` — a current Codex user prompt
//!   (`payload.item.content`). The stable item id is retained as message
//!   identity. Legacy `payload.type == "user_message"` records remain
//!   supported through `payload.message`.
//! * `event_msg` with `payload.type == "agent_message"` — a real assistant reply
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "token_count"` — provider usage captured
//!   by the canonical observation path, not conversational message metadata.
//! * `event_msg` with `payload.type == "thread_goal_updated"` — the structured
//!   session goal and its lifecycle (`payload.goal.{objective,status,tokensUsed,
//!   timeUsedSeconds,createdAt,updatedAt}`). `TraceDecay` records each state as a
//!   compact `goal` row (objective as text, the rest in `metadata_json`) so the
//!   session's goal and whether it is still active is searchable. `status` is
//!   stored verbatim — real rollouts emit `active`/`paused`, but any future
//!   value (e.g. `completed`) is carried through unchanged rather than mapped to
//!   a fixed enum. Consecutive events that repeat the same `(objective, status)`
//!   within one parse pass are deduped; each genuine transition keeps its row.
//! * `compacted` — Codex context-compression boundary. The rollout stores the
//!   replacement history and an encrypted compaction body, so `TraceDecay` records
//!   the boundary/provenance as a summary record without claiming plaintext
//!   access to Codex's private summary.
//! * `response_item` goal context — Codex replays active thread goals as
//!   synthetic user context. `TraceDecay` indexes those as compact goal-context
//!   records so LCM can catalog the objective and budget without treating the
//!   instruction boilerplate as normal conversation.
//! * subagent rollouts — separate `rollout-*.jsonl` files whose leading
//!   `session_meta` has `thread_source == "subagent"` and parent ids in
//!   `forked_from_id` / `source.subagent.thread_spawn.parent_thread_id`.
//!
//! Conversational `response_item` entries are intentionally skipped except for
//! Codex goal context blocks: they usually carry auto-injected synthetic
//! context and duplicate the `item_completed`/legacy message turns, so
//! ingesting them would double-count the conversation. Goal context blocks are
//! cataloged as compact `goal_context` rows because real rollouts often record
//! them only in `response_item` form. This append-only JSONL is read with the
//! shared byte-offset machinery and scoped per turn by the latest Codex cwd
//! context.

mod context;
mod events;
mod goals;
mod meta;
mod observation;
mod records;
#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};
use std::ops::Bound;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use sha2::{Digest, Sha256};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1,
};
use tracedecay_store::ParseOffset;

use context::CodexContextState;
use goals::{codex_goal_event_from_line, goal_context_from_line, goal_event_message};
use meta::session_meta;
use records::{
    compacted_summary_from_line, message_from_line, response_item_goal_context_from_line,
    response_item_tool_event_from_line, timestamp_from_record,
};

use crate::runtime::jsonl_observation_admission::{
    SharedJsonlPathPin, install_shared_jsonl_preparation_authority,
    namespace_replacement_message_ids, pin_shared_jsonl_paths, preflight_and_parse_new,
    reserve_shared_jsonl_bytes, shared_jsonl_preparation_capacity,
};
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptScopeMatcher,
    title_from_messages,
};
use crate::runtime::source::{
    FileDiscoveryLimit, FileDiscoveryReport, ParsedTranscript, SessionDraft,
    TranscriptDiscoveryBounds, TranscriptIngestError, TranscriptIngestResult, TranscriptSource,
    stream_new_jsonl,
};

/// Semantic goal payload plus whether the row came from Codex's authoritative
/// current `item_completed` event rather than its preceding `response_item`.
fn goal_context_dedup_projection(
    message: &crate::runtime::SessionMessageRecord,
) -> Option<(serde_json::Value, bool)> {
    if message.kind.as_deref() != Some("goal_context") {
        return None;
    }
    let metadata =
        serde_json::from_str::<serde_json::Value>(message.metadata_json.as_deref()?).ok()?;
    Some((
        metadata.get("codex_goal")?.clone(),
        metadata
            .get("source_event")
            .and_then(serde_json::Value::as_str)
            == Some("item_completed"),
    ))
}

fn with_paired_response_goal(
    current: &mut crate::runtime::SessionMessageRecord,
    response_message_id: &str,
) -> bool {
    let Some(metadata_json) = current.metadata_json.as_deref() else {
        return false;
    };
    let Ok(mut metadata) = serde_json::from_str::<serde_json::Value>(metadata_json) else {
        return false;
    };
    let serde_json::Value::Object(fields) = &mut metadata else {
        return false;
    };
    fields.insert(
        "paired_response_message_id".to_owned(),
        serde_json::Value::String(response_message_id.to_owned()),
    );
    let Ok(metadata_json) = serde_json::to_string(&metadata) else {
        return false;
    };
    current.metadata_json = Some(metadata_json);
    true
}
#[cfg(test)]
pub(crate) use meta::session_meta_read_count_for_test;
pub use meta::{CodexMeta, session_meta_from_record, turn_context_from_record};
pub use observation::{
    CODEX_HOOK_MAX_NEW_BYTES, CodexJsonlAdmissionProgress,
    try_admit_codex_jsonl_observations_for_profile,
    try_admit_codex_jsonl_observations_for_profile_with_admission,
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation,
    try_admit_codex_jsonl_observations_for_project,
    try_admit_codex_jsonl_observations_for_project_with_admission,
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation,
};

const PROVIDER: &str = "codex";
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` → date dirs add depth.
const MAX_SCAN_DEPTH: u8 = 6;
/// Bound on enumerated transcript directories (a decade of daily Codex
/// directories is ~3700; this cap only guards against pathological trees).
const MAX_BUCKET_DIRS: usize = 8192;
/// Unchanged idle probes use cheap retained identities on every scheduler
/// tick. A full emit-suppressed sweep is intentionally much less frequent so
/// old-file rewrites are eventually detected without continuously rereading
/// the entire corpus while idle.
const IDLE_FULL_VALIDATION_CYCLES: u16 = 256;
/// Retained directories revalidated on *every* idle poll, ahead of the
/// round-robin rotation.
///
/// Creating or removing a transcript changes exactly one directory identity —
/// its parent. A uniform rotation over the whole retained authority therefore
/// hides a brand-new session behind an O(corpus) rotation, so recent-first
/// discovery would only notice today's session after several scheduler ticks
/// on a large corpus. Retained directories are ordered newest-first, so the
/// newest bucket plus its ancestor chain (bounded by [`MAX_SCAN_DEPTH`]) is
/// probed every poll; everything older still rotates.
const IDLE_HOT_DIRECTORIES: usize = MAX_SCAN_DEPTH as usize + 2;

/// Process-retained bounded directory traversal for Codex discovery.
///
/// Durable frontiers are committed only after a whole sweep. This state keeps
/// open `ReadDir` iterators between scheduler ticks, so an oversized directory
/// continues at the next entry without rescanning its prefix. Losing the state
/// on restart merely repeats the unfinished sweep at least once.
#[derive(Default)]
pub struct CodexDiscoveryState {
    scan: Option<CodexRetainedScan>,
    pending: Option<CodexPendingPass>,
    idle: Option<CodexIdleProbe>,
}

#[derive(Clone, Default)]
pub struct CodexDiscoveryHub {
    inner: std::sync::Arc<Mutex<CodexDiscoveryHubState>>,
}

#[derive(Default)]
struct CodexDiscoveryHubState {
    discovery: CodexDiscoveryState,
    discovery_scanning: bool,
    source_key: Option<CodexDiscoverySourceKey>,
    frontier: Option<CodexDiscoveryFrontier>,
    consumers: HashMap<String, CodexDiscoveryConsumerState>,
    replay_indexes: HashMap<CodexDiscoverySourceKey, CodexReplayIndex>,
}

struct CodexDiscoveryConsumerState {
    registrations: usize,
    source_key: Option<CodexDiscoverySourceKey>,
    mode: CodexDiscoveryConsumerMode,
    awaiting_ack: Option<CodexQueuedDiscoveryPass>,
    _memory: Option<ProcessSharedMemoryReservationV1>,
}

enum CodexDiscoveryConsumerMode {
    Shared {
        queue: VecDeque<CodexQueuedDiscoveryPass>,
    },
    Replay {
        generation: u128,
        position: Option<CodexIndexedPath>,
        observed_probe_revision: u128,
    },
}

#[derive(Clone)]
struct CodexQueuedDiscoveryPass {
    base: CodexDiscoveryFrontier,
    pass: std::sync::Arc<CodexDiscoveryPass>,
    indexed_ack: Option<(u128, Option<CodexIndexedPath>, u128)>,
}

enum CodexDiscoveryWork {
    Shared {
        state: CodexDiscoveryState,
        frontier: CodexDiscoveryFrontier,
    },
    Replay {
        source_key: CodexDiscoverySourceKey,
        state: CodexDiscoveryState,
        frontier: CodexDiscoveryFrontier,
        generation: u128,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexIndexedPath {
    root_order: u8,
    path: PathBuf,
}

impl Ord for CodexIndexedPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.root_order
            .cmp(&other.root_order)
            .then_with(|| match self.root_order {
                0 => other.path.cmp(&self.path),
                _ => self.path.cmp(&other.path),
            })
    }
}

impl PartialOrd for CodexIndexedPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct CodexReplayIndex {
    generation: u128,
    building_generation: u128,
    discovery: Option<CodexDiscoveryState>,
    frontier: CodexDiscoveryFrontier,
    scanning: bool,
    retire_when_idle: bool,
    complete: bool,
    rebuilding: bool,
    probe_revision: u128,
    paths: BTreeSet<CodexIndexedPath>,
    building_paths: BTreeSet<CodexIndexedPath>,
    _memory: Vec<ProcessSharedMemoryReservationV1>,
    _building_memory: Vec<ProcessSharedMemoryReservationV1>,
    _scanner_memory: Option<ProcessSharedMemoryReservationV1>,
    completed_enumerations: u64,
    files_considered: u64,
}

impl Default for CodexReplayIndex {
    fn default() -> Self {
        Self {
            generation: 1,
            building_generation: 1,
            discovery: Some(CodexDiscoveryState::default()),
            frontier: CodexDiscoveryFrontier::initial(),
            scanning: false,
            retire_when_idle: false,
            complete: false,
            rebuilding: true,
            probe_revision: 0,
            paths: BTreeSet::new(),
            building_paths: BTreeSet::new(),
            _memory: Vec::new(),
            _building_memory: Vec::new(),
            _scanner_memory: None,
            completed_enumerations: 0,
            files_considered: 0,
        }
    }
}

#[cfg(test)]
static CODEX_REPLAY_INDEX_ENTRIES_VISITED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn reset_replay_index_entries_visited_for_test() {
    CODEX_REPLAY_INDEX_ENTRIES_VISITED.store(0, std::sync::atomic::Ordering::Release);
}

#[cfg(test)]
fn replay_index_entries_visited_for_test() -> u64 {
    CODEX_REPLAY_INDEX_ENTRIES_VISITED.load(std::sync::atomic::Ordering::Acquire)
}

fn indexed_replay_pass(
    index: &CodexReplayIndex,
    bounds: TranscriptDiscoveryBounds,
    frontier: CodexDiscoveryFrontier,
    position: Option<&CodexIndexedPath>,
) -> TranscriptIngestResult<(CodexDiscoveryPass, Option<CodexIndexedPath>)> {
    let mut paths = Vec::new();
    let mut bytes_charged = 0_u64;
    let mut more = false;
    let mut next_position = position.cloned();
    let lower = position.map_or(Bound::Unbounded, Bound::Excluded);
    for indexed in index.paths.range((lower, Bound::Unbounded)) {
        #[cfg(test)]
        CODEX_REPLAY_INDEX_ENTRIES_VISITED.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let path_bytes =
            u64::try_from(crate::runtime::source::path_byte_len(&indexed.path)).unwrap_or(u64::MAX);
        if paths.len() >= bounds.max_files.max(1)
            || (!paths.is_empty()
                && bytes_charged.saturating_add(path_bytes) > bounds.max_discovery_bytes)
        {
            more = true;
            break;
        }
        bytes_charged = bytes_charged.saturating_add(path_bytes);
        paths.push(indexed.path.clone());
        next_position = Some(indexed.clone());
    }
    let complete = index.complete && !more;
    let next_frontier = if complete {
        index.frontier
    } else {
        frontier.for_coverage(false)
    };
    let pin = std::sync::Arc::new(pin_shared_jsonl_paths(&paths));
    Ok((
        CodexDiscoveryPass {
            report: FileDiscoveryReport {
                paths,
                truncated: (!complete).then_some(FileDiscoveryLimit::FileCount),
                skipped_oversized_entries: 0,
                bytes_charged,
                files_considered: 0,
            },
            next_frontier,
            selected_sources: Vec::new(),
            _shared_page_pin: Some(pin),
        },
        next_position,
    ))
}

const MAX_CONSUMER_DISCOVERY_BACKLOG: usize = 1;

pub(crate) enum CodexDiscoveryDelivery {
    Ready(std::sync::Arc<CodexDiscoveryPass>),
    Waiting,
}

impl CodexDiscoveryHub {
    pub fn configure_preparation_resources(
        &self,
        memory: std::sync::Arc<ProcessResidentMemoryV1>,
    ) -> TranscriptIngestResult<()> {
        install_shared_jsonl_preparation_authority(memory)
    }

    pub fn register(&self, consumer: &str, source_home: Option<&Path>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let source_key = source_home
            .map(CodexSource::with_home)
            .or_else(CodexSource::new)
            .map(|source| source.discovery_key());
        if let Some(source_key) = &source_key
            && let Some(index) = inner.replay_indexes.get_mut(source_key)
        {
            index.retire_when_idle = false;
        }
        if let Some(existing) = inner.consumers.get_mut(consumer) {
            existing.registrations = existing.registrations.saturating_add(1);
            return;
        }
        let mode = if inner.frontier.is_none() {
            CodexDiscoveryConsumerMode::Shared {
                queue: VecDeque::new(),
            }
        } else {
            // A consumer joining after shared discovery began must replay from
            // its own durable frontier; inheriting the hub's idle Complete
            // watermark would silently skip its historical corpus.
            CodexDiscoveryConsumerMode::Replay {
                generation: 0,
                position: None,
                observed_probe_revision: 0,
            }
        };
        inner.consumers.insert(
            consumer.to_owned(),
            CodexDiscoveryConsumerState {
                registrations: 1,
                source_key,
                mode,
                awaiting_ack: None,
                _memory: None,
            },
        );
        hotpath::gauge!("codex_discovery_consumers").set(inner.consumers.len() as f64);
    }

    pub fn deregister(&self, consumer: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let remove = match inner.consumers.get_mut(consumer) {
            Some(state) if state.registrations > 1 => {
                state.registrations -= 1;
                false
            }
            Some(_) => true,
            None => false,
        };
        if !remove {
            return;
        }
        let source_key = inner
            .consumers
            .remove(consumer)
            .and_then(|state| state.source_key);
        if let Some(source_key) = source_key {
            let source_is_live = inner
                .consumers
                .values()
                .any(|state| state.source_key.as_ref() == Some(&source_key));
            if !source_is_live {
                let scanning = inner
                    .replay_indexes
                    .get(&source_key)
                    .is_some_and(|index| index.scanning);
                if scanning {
                    if let Some(index) = inner.replay_indexes.get_mut(&source_key) {
                        index.retire_when_idle = true;
                    }
                } else {
                    inner.replay_indexes.remove(&source_key);
                }
            }
        }
        hotpath::gauge!("codex_discovery_consumers").set(inner.consumers.len() as f64);
    }

    #[hotpath::skip]
    pub(crate) async fn discover(
        &self,
        consumer: &str,
        source: &CodexSource,
        bounds: TranscriptDiscoveryBounds,
        frontier: CodexDiscoveryFrontier,
    ) -> TranscriptIngestResult<CodexDiscoveryDelivery> {
        let hub = self.clone();
        let consumer = consumer.to_owned();
        let source = source.clone();
        let delivery = tokio::task::spawn_blocking(move || {
            hub.discover_blocking(&consumer, &source, bounds, frontier)
        })
        .await
        .map_err(|_| TranscriptIngestError::InvalidCodexDiscoveryFrontier {
            detail: "Codex discovery blocking task failed",
        })??;
        if let CodexDiscoveryDelivery::Ready(pass) = &delivery
            && let Some(pin) = &pass._shared_page_pin
        {
            pin.start_prefetches(&pass.report.paths);
        }
        Ok(delivery)
    }

    fn discover_blocking(
        &self,
        consumer: &str,
        source: &CodexSource,
        bounds: TranscriptDiscoveryBounds,
        frontier: CodexDiscoveryFrontier,
    ) -> TranscriptIngestResult<CodexDiscoveryDelivery> {
        loop {
            let work = {
                let mut inner = self.inner.lock().map_err(|_| {
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "Codex discovery hub lock is poisoned",
                    }
                })?;
                let source_key = source.discovery_key();
                let replay = {
                    let consumer_state = inner.consumers.get_mut(consumer).ok_or(
                        TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                            detail: "Codex discovery consumer is not registered",
                        },
                    )?;
                    match &consumer_state.source_key {
                        Some(registered) if registered != &source_key => {
                            return Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                detail: "Codex discovery consumer changed source authority",
                            });
                        }
                        Some(_) => {}
                        None => consumer_state.source_key = Some(source_key.clone()),
                    }
                    if let Some(awaiting) = &consumer_state.awaiting_ack {
                        hotpath::gauge!("codex_discovery_generation_retries").inc(1.0);
                        return Ok(CodexDiscoveryDelivery::Ready(std::sync::Arc::clone(
                            &awaiting.pass,
                        )));
                    }
                    let replay = matches!(
                        consumer_state.mode,
                        CodexDiscoveryConsumerMode::Replay { .. }
                    );
                    if replay && consumer_state._memory.is_none() {
                        consumer_state._memory = reserve_shared_jsonl_bytes(
                            u64::try_from(std::mem::size_of::<CodexDiscoveryConsumerState>())
                                .unwrap_or(u64::MAX)
                                .saturating_add(4096),
                            "Codex replay consumer state capacity",
                        )?;
                    }
                    replay
                };
                if replay {
                    let ready = inner
                        .replay_indexes
                        .get(&source_key)
                        .is_some_and(|index| index.complete && !index.rebuilding);
                    let mut start_probe = false;
                    if ready {
                        let (consumer_generation, consumer_position, observed_probe_revision) =
                            inner
                                .consumers
                                .get(consumer)
                                .and_then(|state| match &state.mode {
                                    CodexDiscoveryConsumerMode::Replay {
                                        generation,
                                        position,
                                        observed_probe_revision,
                                    } => Some((
                                        *generation,
                                        position.clone(),
                                        *observed_probe_revision,
                                    )),
                                    CodexDiscoveryConsumerMode::Shared { .. } => None,
                                })
                                .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                    detail: "Codex replay consumer registration disappeared",
                                })?;
                        let index = inner.replay_indexes.get(&source_key).ok_or(
                            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                detail: "Codex replay index disappeared before delivery",
                            },
                        )?;
                        let effective_position = (consumer_generation == index.generation)
                            .then_some(consumer_position)
                            .flatten();
                        let index_generation = index.generation;
                        let lower = effective_position
                            .as_ref()
                            .map_or(Bound::Unbounded, Bound::Excluded);
                        let has_remaining = index
                            .paths
                            .range((lower, Bound::Unbounded))
                            .next()
                            .is_some();
                        start_probe = !has_remaining
                            && frontier.is_complete()
                            && observed_probe_revision == index.probe_revision;
                        if !start_probe {
                            let index_probe_revision = index.probe_revision;
                            let (pass, next_position) = indexed_replay_pass(
                                index,
                                bounds,
                                frontier,
                                effective_position.as_ref(),
                            )?;
                            let state = inner.consumers.get_mut(consumer).ok_or(
                                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                    detail: "Codex replay consumer registration disappeared",
                                },
                            )?;
                            let CodexDiscoveryConsumerMode::Replay {
                                generation,
                                position,
                                observed_probe_revision: _,
                            } = &mut state.mode
                            else {
                                continue;
                            };
                            if *generation != index_generation {
                                *generation = index_generation;
                                *position = None;
                            }
                            let pass = std::sync::Arc::new(pass);
                            state.awaiting_ack = Some(CodexQueuedDiscoveryPass {
                                base: frontier,
                                pass: std::sync::Arc::clone(&pass),
                                indexed_ack: Some((
                                    index_generation,
                                    next_position,
                                    index_probe_revision,
                                )),
                            });
                            return Ok(CodexDiscoveryDelivery::Ready(pass));
                        }
                    }
                    let index = inner.replay_indexes.entry(source_key.clone()).or_default();
                    if index.scanning {
                        hotpath::gauge!("codex_discovery_scanner_waits").inc(1.0);
                        return Ok(CodexDiscoveryDelivery::Waiting);
                    }
                    if index._scanner_memory.is_none() {
                        index._scanner_memory = reserve_shared_jsonl_bytes(
                            bounds.max_discovery_bytes.saturating_mul(2).saturating_add(
                                u64::try_from(std::mem::size_of::<CodexReplayIndex>())
                                    .unwrap_or(u64::MAX),
                            ),
                            "Codex shared replay scanner state capacity",
                        )?;
                    }
                    let Some(state) = index.discovery.take() else {
                        return Ok(CodexDiscoveryDelivery::Waiting);
                    };
                    index.scanning = true;
                    if start_probe {
                        hotpath::gauge!("codex_discovery_validation_passes").inc(1.0);
                    }
                    CodexDiscoveryWork::Replay {
                        source_key,
                        state,
                        frontier: index.frontier,
                        generation: index.generation,
                    }
                } else {
                    let consumer_state = inner.consumers.get_mut(consumer).ok_or(
                        TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                            detail: "Codex discovery consumer registration disappeared",
                        },
                    )?;
                    let queued = match &mut consumer_state.mode {
                        CodexDiscoveryConsumerMode::Shared { queue } => queue.pop_front(),
                        CodexDiscoveryConsumerMode::Replay { .. } => None,
                    };
                    if let Some(queued) = queued {
                        if queued.base == frontier {
                            consumer_state.awaiting_ack = Some(queued.clone());
                            return Ok(CodexDiscoveryDelivery::Ready(queued.pass));
                        }
                        consumer_state.mode = CodexDiscoveryConsumerMode::Replay {
                            generation: 0,
                            position: None,
                            observed_probe_revision: 0,
                        };
                        continue;
                    }
                    let shared_frontier = *inner.frontier.get_or_insert(frontier);
                    let shared_source = inner.source_key.get_or_insert(source_key.clone());
                    if shared_frontier != frontier || *shared_source != source_key {
                        let consumer_state = inner.consumers.get_mut(consumer).ok_or(
                            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                detail: "Codex discovery consumer registration disappeared",
                            },
                        )?;
                        consumer_state.mode = CodexDiscoveryConsumerMode::Replay {
                            generation: 0,
                            position: None,
                            observed_probe_revision: 0,
                        };
                        continue;
                    }
                    if inner.discovery_scanning {
                        hotpath::gauge!("codex_discovery_scanner_waits").inc(1.0);
                        return Ok(CodexDiscoveryDelivery::Waiting);
                    }
                    inner.discovery_scanning = true;
                    CodexDiscoveryWork::Shared {
                        state: std::mem::take(&mut inner.discovery),
                        frontier: shared_frontier,
                    }
                }
            };

            let (mut discovery, base, replay_generation, replay_source) = match work {
                CodexDiscoveryWork::Shared { state, frontier } => (state, frontier, None, None),
                CodexDiscoveryWork::Replay {
                    source_key,
                    state,
                    frontier,
                    generation,
                } => (state, frontier, Some(generation), Some(source_key)),
            };
            let shared = replay_generation.is_none();
            let bounds = if shared {
                TranscriptDiscoveryBounds {
                    max_files: bounds.max_files.min(shared_jsonl_preparation_capacity()),
                    ..bounds
                }
            } else {
                bounds
            };
            let result = source.discover_transcript_paths_with_state(bounds, base, &mut discovery);
            let mut inner = self.inner.lock().map_err(|_| {
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex discovery hub lock is poisoned",
                }
            })?;
            if shared {
                inner.discovery_scanning = false;
                inner.discovery = discovery;
                let mut pass = result?;
                inner.discovery.acknowledge();
                inner.frontier = Some(pass.next_frontier);
                pass._shared_page_pin = Some(std::sync::Arc::new(pin_shared_jsonl_paths(
                    &pass.report.paths,
                )));
                hotpath::gauge!("codex_discovery_pending_paths")
                    .set(pass.report.paths.len() as f64);
                hotpath::gauge!("codex_discovery_pending_bytes")
                    .set(pass.report.bytes_charged as f64);
                let pass = std::sync::Arc::new(pass);
                let queued = CodexQueuedDiscoveryPass {
                    base,
                    pass: std::sync::Arc::clone(&pass),
                    indexed_ack: None,
                };
                let shared_source_key = inner.source_key.clone();
                for state in inner.consumers.values_mut() {
                    let CodexDiscoveryConsumerMode::Shared { queue } = &mut state.mode else {
                        continue;
                    };
                    if state.source_key.as_ref() != shared_source_key.as_ref() {
                        continue;
                    }
                    if queue.len() >= MAX_CONSUMER_DISCOVERY_BACKLOG {
                        state.mode = CodexDiscoveryConsumerMode::Replay {
                            generation: 0,
                            position: None,
                            observed_probe_revision: 0,
                        };
                        state.awaiting_ack = None;
                    } else {
                        queue.push_back(queued.clone());
                    }
                }
                drop(inner);
                continue;
            }

            let source_key =
                replay_source.ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay source authority is missing",
                })?;
            let generation =
                replay_generation.ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay generation authority is missing",
                })?;
            let retire_source = inner
                .replay_indexes
                .get(&source_key)
                .is_some_and(|index| index.retire_when_idle)
                && !inner
                    .consumers
                    .values()
                    .any(|state| state.source_key.as_ref() == Some(&source_key));
            if retire_source {
                inner.replay_indexes.remove(&source_key);
                return Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay source retired while scanning",
                });
            }
            let index = inner.replay_indexes.get_mut(&source_key).ok_or(
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay index disappeared while scanning",
                },
            )?;
            if index.generation != generation {
                return Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay index generation changed while scanning",
                });
            }
            index.scanning = false;
            index.discovery = Some(discovery);
            let pass = result?;
            let changed = pass.next_frontier != index.frontier;
            let emitting_changed_corpus = index
                .discovery
                .as_ref()
                .and_then(|state| state.scan.as_ref())
                .is_some_and(|scan| !scan.validation);
            if !index.rebuilding && (changed || emitting_changed_corpus) {
                index.building_generation = index.generation.checked_add(1).ok_or(
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "Codex replay index generation exhausted",
                    },
                )?;
                index.rebuilding = true;
                index.building_paths.clear();
                index._building_memory.clear();
            }
            let additions = pass
                .report
                .paths
                .iter()
                .map(|path| CodexIndexedPath {
                    root_order: if path.starts_with(&source.sessions_dir) {
                        0
                    } else {
                        1
                    },
                    path: path.clone(),
                })
                .filter(|path| index.rebuilding && !index.building_paths.contains(path))
                .collect::<Vec<_>>();
            let retained_bytes = additions.iter().fold(0_u64, |total, path| {
                total.saturating_add(
                    u64::try_from(crate::runtime::source::path_byte_len(&path.path))
                        .unwrap_or(u64::MAX)
                        .saturating_add(
                            u64::try_from(std::mem::size_of::<CodexIndexedPath>())
                                .unwrap_or(u64::MAX),
                        ),
                )
            });
            if retained_bytes != 0 {
                let reservation = reserve_shared_jsonl_bytes(
                    retained_bytes,
                    "Codex shared replay path index capacity",
                )?
                .ok_or(TranscriptIngestError::BackgroundResourceUnavailable {
                    provider: PROVIDER,
                    resource: "Codex shared replay path index capacity",
                })?;
                index._building_memory.push(reservation);
            }
            index.building_paths.extend(additions);
            index
                .discovery
                .as_mut()
                .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "Codex replay discovery state disappeared before acknowledgement",
                })?
                .acknowledge();
            let completed_probe_cycle = index
                .discovery
                .as_ref()
                .and_then(|state| state.idle.as_ref())
                .is_some_and(|idle| idle.next == 0);
            let completed_sweep = index
                .discovery
                .as_ref()
                .is_some_and(|state| state.scan.is_none() && state.idle.is_some());
            index.frontier = pass.next_frontier;
            if pass.next_frontier.is_complete() && completed_sweep {
                if index.rebuilding {
                    std::mem::swap(&mut index.paths, &mut index.building_paths);
                    std::mem::swap(&mut index._memory, &mut index._building_memory);
                    index.building_paths.clear();
                    index._building_memory.clear();
                    index.generation = index.building_generation;
                    index.rebuilding = false;
                    index.complete = true;
                    index.completed_enumerations = index
                        .completed_enumerations
                        .checked_add(1)
                        .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                            detail: "Codex replay enumeration count exhausted",
                        })?;
                    index.files_considered = pass.report.files_considered;
                } else if completed_probe_cycle {
                    index.probe_revision = index.probe_revision.checked_add(1).ok_or(
                        TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                            detail: "Codex replay probe revision exhausted",
                        },
                    )?;
                }
            }
            return Ok(CodexDiscoveryDelivery::Waiting);
        }
    }

    pub(crate) fn acknowledge(&self, consumer: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let shared_frontier = inner.frontier;
        let shared_source_key = inner.source_key.clone();
        let Some(state) = inner.consumers.get_mut(consumer) else {
            return;
        };
        let Some(acknowledged) = state.awaiting_ack.take() else {
            return;
        };
        if let Some((generation, position, probe_revision)) = acknowledged.indexed_ack
            && let CodexDiscoveryConsumerMode::Replay {
                generation: consumer_generation,
                position: consumer_position,
                observed_probe_revision,
            } = &mut state.mode
            && *consumer_generation == generation
        {
            *consumer_position = position;
            *observed_probe_revision = probe_revision;
            if acknowledged.pass.next_frontier.is_complete()
                && shared_frontier == Some(acknowledged.pass.next_frontier)
                && state.source_key == shared_source_key
            {
                state.mode = CodexDiscoveryConsumerMode::Shared {
                    queue: VecDeque::new(),
                };
            }
        }
    }
}

struct CodexPendingPass {
    pass: CodexDiscoveryPass,
    sources: Vec<CodexFileIdentity>,
}

struct CodexIdleProbe {
    directories: Vec<CodexDirectoryIdentity>,
    active_files: Vec<CodexFileIdentity>,
    next: usize,
    epoch: CodexCorpusEpoch,
    complete: bool,
    completed_probe_cycles: u16,
}

struct CodexRetainedScan {
    phase: CodexScanPhase,
    directories: Vec<CodexDirectoryIdentity>,
    epoch: CodexCorpusEpoch,
    complete: bool,
    skipped_oversized_entries: u64,
    files_considered: u64,
    active_files: BinaryHeap<Reverse<CodexFileIdentity>>,
    directory_bytes: u64,
    validation: bool,
}

enum CodexScanPhase {
    Directories {
        queued: VecDeque<(PathBuf, u8, u8, u64)>,
        current: Option<(PathBuf, u8, u8, std::fs::ReadDir)>,
    },
    Files {
        next_directory: usize,
        current: Option<(usize, std::fs::ReadDir)>,
        deferred: Option<Box<(PathBuf, std::fs::Metadata)>>,
        resume_directories: Option<CodexDirectoryResume>,
    },
}

struct CodexDirectoryResume {
    queued: VecDeque<(PathBuf, u8, u8, u64)>,
    current: Option<(PathBuf, u8, u8, std::fs::ReadDir)>,
}

#[derive(Clone)]
struct CodexDirectoryIdentity {
    path: PathBuf,
    root_order: u8,
    identity: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexFileIdentity {
    path: PathBuf,
    identity: [u8; 32],
}

impl Ord for CodexFileIdentity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path.cmp(&other.path)
    }
}

impl PartialOrd for CodexFileIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl CodexDiscoveryState {
    pub fn acknowledge(&mut self) {
        self.pending = None;
    }

    fn reset(&mut self, source: &CodexSource) {
        self.reset_for(source, false);
    }

    fn reset_for(&mut self, source: &CodexSource, validation: bool) {
        self.scan = Some(CodexRetainedScan::new(source, validation));
        self.pending = None;
        self.idle = None;
    }
}

impl CodexRetainedScan {
    fn new(source: &CodexSource, validation: bool) -> Self {
        Self {
            phase: CodexScanPhase::Directories {
                queued: VecDeque::from([
                    (source.sessions_dir.clone(), 0, 0, 0),
                    (source.archived_sessions_dir.clone(), 0, 1, 0),
                ]),
                current: None,
            },
            directories: Vec::new(),
            epoch: CodexCorpusEpoch::initial(),
            complete: true,
            skipped_oversized_entries: 0,
            files_considered: 0,
            active_files: BinaryHeap::new(),
            directory_bytes: 0,
            validation,
        }
    }
}

#[derive(Clone)]
pub struct CodexSource {
    sessions_dir: PathBuf,
    archived_sessions_dir: PathBuf,
    user_scope: Option<UserCodexScope>,
    /// Source-lifetime cache of project-root matchers and cwd worktree
    /// resolutions, so one scan pass runs git identity discovery once per
    /// root/cwd instead of once per transcript record.
    project_matchers: ProjectRootMatcherCache,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodexDiscoverySourceKey {
    sessions_dir: PathBuf,
    archived_sessions_dir: PathBuf,
}

const EXACT_HOOK_DISCOVERY_UNITS_PER_CALL: usize = 64;
const MAX_EXACT_HOOK_SOURCE_AUTHORITIES: usize = 8;
const MAX_EXACT_HOOK_SESSION_REQUESTS: usize = 64;

pub(crate) struct CodexExactSessionLookupOutcome {
    pub paths: Vec<PathBuf>,
    pub source_deferred: bool,
    #[cfg(test)]
    pub files_considered: u64,
}

struct CodexExactSessionPathAuthority {
    sources: VecDeque<CodexExactSessionSourceState>,
    next_lease: u128,
}

struct CodexExactSessionSourceState {
    key: CodexDiscoverySourceKey,
    lease: u128,
    discovery: Option<CodexDiscoveryState>,
    frontier: CodexDiscoveryFrontier,
    indexed_targets: HashSet<PathBuf>,
    indexed_by_session: HashMap<String, Vec<PathBuf>>,
    indexed_path_bytes: u64,
    requested: VecDeque<CodexExactSessionRequest>,
    _memory: Option<ProcessSharedMemoryReservationV1>,
}

struct CodexExactSessionRequest {
    session_id: String,
    lease: u128,
    paths: Vec<PathBuf>,
    completed: bool,
    _memory: Option<ProcessSharedMemoryReservationV1>,
}

static CODEX_EXACT_SESSION_PATH_AUTHORITY: OnceLock<Mutex<CodexExactSessionPathAuthority>> =
    OnceLock::new();

impl Default for CodexExactSessionPathAuthority {
    fn default() -> Self {
        Self {
            sources: VecDeque::new(),
            next_lease: 1,
        }
    }
}

impl CodexExactSessionPathAuthority {
    fn issue_lease(&mut self) -> TranscriptIngestResult<u128> {
        let lease = self.next_lease;
        self.next_lease = self.next_lease.checked_add(1).ok_or(
            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "exact Codex lookup lease authority exhausted",
            },
        )?;
        Ok(lease)
    }

    fn source_for_lease_mut(
        &mut self,
        key: &CodexDiscoverySourceKey,
        lease: u128,
        detail: &'static str,
    ) -> TranscriptIngestResult<&mut CodexExactSessionSourceState> {
        self.sources
            .iter_mut()
            .find(|entry| &entry.key == key && entry.lease == lease)
            .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier { detail })
    }

    fn source_index_or_admit(
        &mut self,
        key: CodexDiscoverySourceKey,
    ) -> TranscriptIngestResult<usize> {
        if let Some(index) = self.sources.iter().position(|entry| entry.key == key) {
            return Ok(index);
        }
        if self.sources.len() >= MAX_EXACT_HOOK_SOURCE_AUTHORITIES {
            let Some(completed) = self.sources.iter().position(|source| {
                !source.requested.is_empty()
                    && source.requested.iter().all(|request| request.completed)
            }) else {
                return Err(TranscriptIngestError::BackgroundResourceUnavailable {
                    provider: PROVIDER,
                    resource: "exact-session source lookup capacity",
                });
            };
            self.sources.remove(completed);
        }
        let source_bytes =
            TranscriptDiscoveryBounds::from_discovered_units(EXACT_HOOK_DISCOVERY_UNITS_PER_CALL)
                .max_discovery_bytes
                .saturating_mul(2)
                .saturating_add(
                    u64::try_from(std::mem::size_of::<CodexExactSessionSourceState>())
                        .unwrap_or(u64::MAX),
                );
        let memory = reserve_shared_jsonl_bytes(
            source_bytes,
            "exact-session source lookup resident-memory capacity",
        )?;
        let lease = self.issue_lease()?;
        self.sources.push_back(CodexExactSessionSourceState {
            key,
            lease,
            discovery: Some(CodexDiscoveryState::default()),
            frontier: CodexDiscoveryFrontier::initial(),
            indexed_targets: HashSet::new(),
            indexed_by_session: HashMap::new(),
            indexed_path_bytes: 0,
            requested: VecDeque::new(),
            _memory: memory,
        });
        Ok(self.sources.len().saturating_sub(1))
    }

    fn request_index_or_admit(
        &mut self,
        source_index: usize,
        session_id: String,
    ) -> TranscriptIngestResult<usize> {
        let source = self.sources.get_mut(source_index).ok_or(
            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "exact Codex source authority disappeared",
            },
        )?;
        if let Some(index) = source
            .requested
            .iter()
            .position(|request| request.session_id == session_id)
        {
            return Ok(index);
        }
        if source.requested.len() >= MAX_EXACT_HOOK_SESSION_REQUESTS {
            let Some(completed) = source
                .requested
                .iter()
                .position(|request| request.completed)
            else {
                return Err(TranscriptIngestError::BackgroundResourceUnavailable {
                    provider: PROVIDER,
                    resource: "exact-session request lookup capacity",
                });
            };
            source.requested.remove(completed);
        }
        let request_bytes =
            TranscriptDiscoveryBounds::from_discovered_units(EXACT_HOOK_DISCOVERY_UNITS_PER_CALL)
                .max_discovery_bytes
                .saturating_add(u64::try_from(session_id.capacity()).unwrap_or(u64::MAX))
                .saturating_add(
                    u64::try_from(std::mem::size_of::<CodexExactSessionRequest>())
                        .unwrap_or(u64::MAX),
                );
        let memory = reserve_shared_jsonl_bytes(
            request_bytes,
            "exact-session request lookup resident-memory capacity",
        )?;
        let lease = self.issue_lease()?;
        let source = self.sources.get_mut(source_index).ok_or(
            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "exact Codex source authority disappeared",
            },
        )?;
        let paths = source
            .indexed_by_session
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        source.requested.push_back(CodexExactSessionRequest {
            session_id,
            lease,
            completed: !paths.is_empty() || source.frontier.is_complete(),
            paths,
            _memory: memory,
        });
        Ok(source.requested.len().saturating_sub(1))
    }
}

#[derive(Clone)]
struct UserCodexScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl CodexSource {
    fn discovery_key(&self) -> CodexDiscoverySourceKey {
        CodexDiscoverySourceKey {
            sessions_dir: self.sessions_dir.clone(),
            archived_sessions_dir: self.archived_sessions_dir.clone(),
        }
    }

    /// Advances one bounded retained discovery slice and resolves the exact
    /// rollout filename used by a Codex hook event. Repeated calls continue the
    /// same iterator instead of rereading the corpus prefix.
    #[hotpath::measure(label = "sessions.hosts.codex.find_session")]
    pub(crate) fn find_session_transcript_paths_bounded(
        &self,
        session_id: &str,
    ) -> TranscriptIngestResult<CodexExactSessionLookupOutcome> {
        let authority = CODEX_EXACT_SESSION_PATH_AUTHORITY
            .get_or_init(|| Mutex::new(CodexExactSessionPathAuthority::default()));
        let key = self.discovery_key();
        let (source_lease, request_lease, cached_paths) = {
            let mut authority = authority.lock().map_err(|_| {
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex session path authority lock is poisoned",
                }
            })?;
            let index = authority.source_index_or_admit(key.clone())?;
            let request_index = authority.request_index_or_admit(index, session_id.to_owned())?;
            let state = authority.sources.get_mut(index).ok_or(
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex source authority disappeared",
                },
            )?;
            let request = state.requested.get_mut(request_index).ok_or(
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex request authority disappeared",
                },
            )?;
            (state.lease, request.lease, request.paths.clone())
        };
        if !cached_paths.is_empty() {
            let existing = cached_paths
                .iter()
                .filter(|path| path.exists())
                .cloned()
                .collect::<HashSet<_>>();
            let cached = cached_paths.into_iter().collect::<HashSet<_>>();
            let mut authority = authority.lock().map_err(|_| {
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex session path authority lock is poisoned",
                }
            })?;
            let state = authority.source_for_lease_mut(
                &key,
                source_lease,
                "exact Codex source lease expired during cached-path validation",
            )?;
            let request = state
                .requested
                .iter_mut()
                .find(|request| request.session_id == session_id && request.lease == request_lease)
                .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex request lease expired during cached-path validation",
                })?;
            request
                .paths
                .retain(|path| !cached.contains(path) || existing.contains(path));
            if !request.paths.is_empty() {
                return Ok(CodexExactSessionLookupOutcome {
                    paths: request.paths.clone(),
                    source_deferred: false,
                    #[cfg(test)]
                    files_considered: 0,
                });
            }
            request.completed = false;
        }
        let (mut discovery, frontier) = {
            let mut authority = authority.lock().map_err(|_| {
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex session path authority lock is poisoned",
                }
            })?;
            let state = authority.source_for_lease_mut(
                &key,
                source_lease,
                "exact Codex source lease expired before scanning",
            )?;
            let request = state
                .requested
                .iter_mut()
                .find(|request| request.session_id == session_id && request.lease == request_lease)
                .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex request lease expired before scanning",
                })?;
            request.completed = false;
            let Some(discovery) = state.discovery.take() else {
                return Ok(CodexExactSessionLookupOutcome {
                    paths: Vec::new(),
                    source_deferred: true,
                    #[cfg(test)]
                    files_considered: 0,
                });
            };
            (discovery, state.frontier)
        };

        let bounds =
            TranscriptDiscoveryBounds::from_discovered_units(EXACT_HOOK_DISCOVERY_UNITS_PER_CALL);
        #[cfg(test)]
        let files_considered_before = discovery
            .scan
            .as_ref()
            .map_or(0, |scan| scan.files_considered);
        let result = self.discover_transcript_paths_with_state(bounds, frontier, &mut discovery);
        let pass = match result {
            Ok(pass) => pass,
            Err(error) => {
                let mut authority = authority.lock().map_err(|_| {
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "exact Codex session path authority lock is poisoned",
                    }
                })?;
                if let Ok(state) = authority.source_for_lease_mut(
                    &key,
                    source_lease,
                    "exact Codex source lease expired while restoring failed scan",
                ) {
                    state.discovery = Some(discovery);
                }
                return Err(error);
            }
        };
        let canonical_candidates = pass
            .report
            .paths
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .map(|name| (name.to_owned(), path))
            })
            .map(|(name, path)| {
                std::fs::canonicalize(path)
                    .map(|canonical| (name, canonical))
                    .map_err(|source| TranscriptIngestError::ScanIo {
                        operation: "resolve exact Codex session candidate",
                        path: path.clone(),
                        source,
                    })
            })
            .collect::<TranscriptIngestResult<Vec<_>>>();
        let canonical_candidates = match canonical_candidates {
            Ok(candidates) => candidates,
            Err(error) => {
                let mut authority = authority.lock().map_err(|_| {
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "exact Codex session path authority lock is poisoned",
                    }
                })?;
                if let Ok(state) = authority.source_for_lease_mut(
                    &key,
                    source_lease,
                    "exact Codex source lease expired while restoring canonicalization failure",
                ) {
                    state.discovery = Some(discovery);
                }
                return Err(error);
            }
        };
        let mut authority =
            authority
                .lock()
                .map_err(|_| TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex session path authority lock is poisoned",
                })?;
        let state = authority.source_for_lease_mut(
            &key,
            source_lease,
            "exact Codex source lease expired while scanning",
        )?;
        let request_bounds =
            TranscriptDiscoveryBounds::from_discovered_units(EXACT_HOOK_DISCOVERY_UNITS_PER_CALL);
        let mut indexed_additions: Vec<(PathBuf, Vec<String>)> = Vec::new();
        let mut indexed_addition_targets = HashSet::new();
        let mut indexed_path_bytes = state.indexed_path_bytes;
        let mut additions: Vec<(usize, PathBuf)> = Vec::new();
        for (name, canonical) in canonical_candidates {
            if !state.indexed_targets.contains(&canonical)
                && indexed_addition_targets.insert(canonical.clone())
            {
                let path_bytes = u64::try_from(crate::runtime::source::path_byte_len(&canonical))
                    .unwrap_or(u64::MAX);
                let next_indexed_path_bytes = indexed_path_bytes.saturating_add(path_bytes);
                if next_indexed_path_bytes > request_bounds.max_discovery_bytes {
                    state.discovery = Some(discovery);
                    return Err(TranscriptIngestError::BackgroundResourceUnavailable {
                        provider: PROVIDER,
                        resource: "exact-session source path index capacity",
                    });
                }
                indexed_path_bytes = next_indexed_path_bytes;
                let mut session_keys = Vec::new();
                if let Some(stem) = name
                    .strip_prefix("rollout-")
                    .and_then(|value| value.strip_suffix(".jsonl"))
                {
                    session_keys.push(stem.to_owned());
                    if stem.len() >= 36 {
                        let candidate = &stem[stem.len() - 36..];
                        if candidate.as_bytes().get(8) == Some(&b'-')
                            && candidate.as_bytes().get(13) == Some(&b'-')
                            && candidate.as_bytes().get(18) == Some(&b'-')
                            && candidate.as_bytes().get(23) == Some(&b'-')
                            && candidate != stem
                        {
                            session_keys.push(candidate.to_owned());
                        }
                    }
                }
                indexed_additions.push((canonical.clone(), session_keys));
            }
            let matching = state
                .requested
                .iter()
                .enumerate()
                .filter_map(|(index, request)| name.contains(&request.session_id).then_some(index))
                .collect::<Vec<_>>();
            for request_index in matching {
                let request = state.requested.get(request_index).ok_or(
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "exact Codex request authority disappeared while matching",
                    },
                )?;
                if request.paths.contains(&canonical)
                    || additions
                        .iter()
                        .any(|(index, path)| *index == request_index && path == &canonical)
                {
                    continue;
                }
                let retained_paths = request
                    .paths
                    .iter()
                    .map(|path| crate::runtime::source::path_byte_len(path))
                    .chain(additions.iter().filter_map(|(index, path)| {
                        (*index == request_index)
                            .then_some(crate::runtime::source::path_byte_len(path))
                    }))
                    .fold(0_usize, usize::saturating_add);
                let next_path_bytes = retained_paths
                    .saturating_add(crate::runtime::source::path_byte_len(&canonical));
                let retained_count = request.paths.len().saturating_add(
                    additions
                        .iter()
                        .filter(|(index, _)| *index == request_index)
                        .count(),
                );
                if retained_count >= request_bounds.max_files
                    || u64::try_from(next_path_bytes).unwrap_or(u64::MAX)
                        > request_bounds.max_discovery_bytes
                {
                    state.discovery = Some(discovery);
                    return Err(TranscriptIngestError::BackgroundResourceUnavailable {
                        provider: PROVIDER,
                        resource: "exact-session retained path capacity",
                    });
                }
                additions.push((request_index, canonical.clone()));
            }
        }
        for (canonical, session_keys) in indexed_additions {
            state.indexed_targets.insert(canonical.clone());
            for session_key in session_keys {
                let paths = state.indexed_by_session.entry(session_key).or_default();
                if let Err(index) = paths.binary_search(&canonical) {
                    paths.insert(index, canonical.clone());
                }
            }
        }
        state.indexed_path_bytes = indexed_path_bytes;
        for (request_index, canonical) in additions {
            let request = state.requested.get_mut(request_index).ok_or(
                TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                    detail: "exact Codex request authority disappeared while publishing",
                },
            )?;
            if let Err(index) = request.paths.binary_search(&canonical) {
                request.paths.insert(index, canonical);
            }
            request.completed = true;
        }
        discovery.acknowledge();
        state.frontier = pass.next_frontier;
        state.discovery = Some(discovery);
        if pass.next_frontier.is_complete() {
            for request in &mut state.requested {
                request.completed = true;
            }
        }
        let paths = state
            .requested
            .iter()
            .find(|request| request.session_id == session_id && request.lease == request_lease)
            .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "exact Codex request lease expired while publishing",
            })?
            .paths
            .clone();
        Ok(CodexExactSessionLookupOutcome {
            source_deferred: paths.is_empty() && !pass.next_frontier.is_complete(),
            paths,
            #[cfg(test)]
            files_considered: pass
                .report
                .files_considered
                .saturating_sub(files_considered_before),
        })
    }

    /// Source rooted at the real `~/.codex`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.codex` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        let codex_home = home.join(".codex");
        Self {
            sessions_dir: codex_home.join("sessions"),
            archived_sessions_dir: codex_home.join("archived_sessions"),
            user_scope: None,
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    /// Restricts ingestion to sessions that cannot be attributed to a registered project.
    #[must_use]
    pub fn for_user_scope(
        mut self,
        session_id: Option<String>,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        self.user_scope = Some(UserCodexScope {
            session_id,
            registered_roots,
        });
        self
    }

    /// Bounded discovery for long-lived schedulers. The caller must
    /// acknowledge only after every returned path was dispositioned and any
    /// advanced durable frontier was persisted.
    #[hotpath::measure(label = "sessions.hosts.codex.discover")]
    pub(crate) fn discover_transcript_paths_with_state(
        &self,
        bounds: TranscriptDiscoveryBounds,
        frontier: CodexDiscoveryFrontier,
        state: &mut CodexDiscoveryState,
    ) -> TranscriptIngestResult<CodexDiscoveryPass> {
        if let Some(pending) = &state.pending {
            let mut valid = true;
            for source in &pending.sources {
                hotpath::gauge!("codex_discovery_pending_revalidations").inc(1.0);
                let metadata = match std::fs::metadata(&source.path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        valid = false;
                        break;
                    }
                    Err(source_error) => {
                        return Err(TranscriptIngestError::ScanIo {
                            operation: "revalidate pending Codex transcript",
                            path: source.path.clone(),
                            source: source_error,
                        });
                    }
                };
                if !metadata.is_file()
                    || codex_corpus_identity(&source.path, &metadata)? != source.identity
                {
                    valid = false;
                    break;
                }
            }
            if valid {
                let mut pass = pending.pass.clone();
                if !pass.next_frontier.is_complete() {
                    pass.next_frontier = frontier.for_coverage(false);
                }
                return Ok(pass);
            }
            hotpath::gauge!("codex_discovery_pending_revalidation_misses").inc(1.0);
            state.reset(self);
        }
        let work_limit = bounds.max_files.max(1);
        let mut restart_idle = None;
        if let Some(idle) = &mut state.idle {
            let mut changed = false;
            let mut completed_cycle = false;
            let authority_len = idle
                .directories
                .len()
                .saturating_add(idle.active_files.len());
            // Hot prefix first: the newest bucket and its ancestors are where a
            // new session lands, and only the parent directory's identity moves
            // when one appears. Rotation alone would defer that discovery by a
            // whole cycle, so probe the hot prefix on every poll.
            let hot = idle.directories.len().min(IDLE_HOT_DIRECTORIES);
            for directory in idle.directories.iter().take(hot) {
                if codex_directory_identity(&directory.path)? != directory.identity {
                    changed = true;
                    break;
                }
            }
            let probes = if changed {
                0
            } else {
                work_limit.min(authority_len.max(1))
            };
            for _ in 0..probes {
                if authority_len == 0 {
                    break;
                }
                let index = idle.next % authority_len;
                idle.next = if index + 1 == authority_len {
                    completed_cycle = true;
                    0
                } else {
                    index + 1
                };
                if let Some(directory) = idle.directories.get(index) {
                    if codex_directory_identity(&directory.path)? != directory.identity {
                        changed = true;
                        break;
                    }
                } else {
                    let file = &idle.active_files[index - idle.directories.len()];
                    let metadata = match std::fs::metadata(&file.path) {
                        Ok(metadata) => metadata,
                        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                            changed = true;
                            break;
                        }
                        Err(source) => {
                            return Err(TranscriptIngestError::ScanIo {
                                operation: "stat retained Codex active transcript",
                                path: file.path.clone(),
                                source,
                            });
                        }
                    };
                    if !metadata.is_file()
                        || codex_corpus_identity(&file.path, &metadata)? != file.identity
                    {
                        changed = true;
                        break;
                    }
                }
            }
            if !changed {
                if completed_cycle {
                    idle.completed_probe_cycles = idle.completed_probe_cycles.saturating_add(1);
                    if idle.completed_probe_cycles >= IDLE_FULL_VALIDATION_CYCLES {
                        restart_idle = Some(true);
                    } else {
                        let next_frontier = if idle.complete {
                            CodexDiscoveryFrontier::complete(idle.epoch)
                        } else {
                            CodexDiscoveryFrontier::in_progress(idle.epoch)
                        };
                        return Ok(CodexDiscoveryPass {
                            report: FileDiscoveryReport {
                                paths: Vec::new(),
                                truncated: (!idle.complete)
                                    .then_some(FileDiscoveryLimit::FileCount),
                                skipped_oversized_entries: 0,
                                bytes_charged: 0,
                                files_considered: 0,
                            },
                            next_frontier,
                            selected_sources: Vec::new(),
                            _shared_page_pin: None,
                        });
                    }
                } else {
                    let next_frontier = if idle.complete {
                        CodexDiscoveryFrontier::complete(idle.epoch)
                    } else {
                        CodexDiscoveryFrontier::in_progress(idle.epoch)
                    };
                    return Ok(CodexDiscoveryPass {
                        report: FileDiscoveryReport {
                            paths: Vec::new(),
                            truncated: (!idle.complete).then_some(FileDiscoveryLimit::FileCount),
                            skipped_oversized_entries: 0,
                            bytes_charged: 0,
                            files_considered: 0,
                        },
                        next_frontier,
                        selected_sources: Vec::new(),
                        _shared_page_pin: None,
                    });
                }
            } else {
                // The probe only proves *that* the corpus moved, not *where*.
                // Restarting straight into an emit sweep hands back whatever
                // the first `max_files` directory entries happen to be, so a
                // session created in a bucket that already holds more files
                // than the pass cap could be starved behind arbitrary
                // same-bucket siblings. Restart into a validation sweep: it
                // measures the whole corpus and reports the newest retained
                // window, and the emit sweep it hands off to still covers the
                // rest across later passes.
                restart_idle = Some(true);
            }
        }
        if let Some(validation) = restart_idle {
            state.reset_for(self, validation);
        }
        if state.scan.is_none() {
            state.reset_for(self, frontier.is_complete());
        }
        if state.scan.as_ref().is_some_and(|scan| scan.validation) {
            hotpath::gauge!("codex_discovery_validation_passes").inc(1.0);
        } else {
            hotpath::gauge!("codex_discovery_emit_passes").inc(1.0);
        }
        let pass = retained_scan_step(self, state, bounds, frontier)?;
        let sources = pass.selected_sources.clone();
        state.pending = Some(CodexPendingPass {
            pass: pass.clone(),
            sources,
        });
        Ok(pass)
    }

    /// One bounded standalone discovery pass.
    ///
    /// Standalone callers intentionally do not claim durable completion when a
    /// tree exceeds one pass. Long-lived schedulers use
    /// [`Self::discover_transcript_paths_with_state`] to retain the directory
    /// iterators and converge without rescanning a prefix.
    pub(crate) fn discover_transcript_paths_with_frontier(
        &self,
        bounds: TranscriptDiscoveryBounds,
        frontier: CodexDiscoveryFrontier,
    ) -> TranscriptIngestResult<CodexDiscoveryPass> {
        let mut state = CodexDiscoveryState::default();
        self.discover_transcript_paths_with_state(bounds, frontier, &mut state)
    }
}

const CODEX_FRONTIER_SWEEPING_V2: u64 = 2;
const CODEX_FRONTIER_COMPLETE_V2: u64 = 3;

/// Versioned durable Codex discovery authority.
///
/// The frontier and epoch use separate `ParseOffset` records. The frontier's
/// numeric fields are zero and `file_id` is the V2 sweep discriminant. The
/// epoch record is an independent 128-bit commutative corpus digest plus exact
/// file count. No authority field is packed, wrapped, masked, or clamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodexDiscoveryFrontier {
    epoch: CodexCorpusEpoch,
    state: CodexDiscoverySweep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexDiscoverySweep {
    InProgress,
    Complete,
}

impl CodexDiscoveryFrontier {
    #[hotpath::skip]
    pub(crate) const fn initial() -> Self {
        Self {
            epoch: CodexCorpusEpoch::initial(),
            state: CodexDiscoverySweep::InProgress,
        }
    }

    #[hotpath::skip]
    const fn in_progress(epoch: CodexCorpusEpoch) -> Self {
        Self {
            epoch,
            state: CodexDiscoverySweep::InProgress,
        }
    }

    #[hotpath::skip]
    const fn complete(epoch: CodexCorpusEpoch) -> Self {
        Self {
            epoch,
            state: CodexDiscoverySweep::Complete,
        }
    }

    #[hotpath::skip]
    pub(crate) const fn is_complete(self) -> bool {
        matches!(self.state, CodexDiscoverySweep::Complete)
    }

    #[hotpath::skip]
    pub(crate) const fn for_coverage(self, coverage_complete: bool) -> Self {
        if self.is_complete() && !coverage_complete {
            Self::in_progress(self.epoch)
        } else {
            self
        }
    }

    pub(crate) fn from_parse_offsets(
        frontier: ParseOffset,
        epoch: ParseOffset,
    ) -> TranscriptIngestResult<Self> {
        let epoch = CodexCorpusEpoch::from_parse_offset(epoch);
        let decoded = match frontier.file_id {
            0 if frontier == ParseOffset::default() => Ok(Self::initial()),
            CODEX_FRONTIER_SWEEPING_V2 if frontier.byte_offset == 0 && frontier.mtime == 0 => {
                Ok(Self::in_progress(epoch))
            }
            CODEX_FRONTIER_COMPLETE_V2 if frontier.byte_offset == 0 && frontier.mtime == 0 => {
                Ok(Self::complete(epoch))
            }
            _ => Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "unknown durable frontier version or state",
            }),
        }?;
        Ok(decoded)
    }

    #[hotpath::skip]
    pub(crate) const fn into_parse_offsets(self) -> (ParseOffset, ParseOffset) {
        if self.epoch.is_initial() && matches!(self.state, CodexDiscoverySweep::InProgress) {
            return (
                ParseOffset {
                    byte_offset: 0,
                    mtime: 0,
                    file_id: 0,
                },
                ParseOffset {
                    byte_offset: 0,
                    mtime: 0,
                    file_id: 0,
                },
            );
        }
        let state = match self.state {
            CodexDiscoverySweep::InProgress => CODEX_FRONTIER_SWEEPING_V2,
            CodexDiscoverySweep::Complete => CODEX_FRONTIER_COMPLETE_V2,
        };
        (
            ParseOffset {
                byte_offset: 0,
                mtime: 0,
                file_id: state,
            },
            self.epoch.into_parse_offset(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodexCorpusEpoch {
    high: u64,
    low: u64,
    files: u64,
}

impl CodexCorpusEpoch {
    #[hotpath::skip]
    const fn initial() -> Self {
        Self {
            high: 0,
            low: 0,
            files: 0,
        }
    }

    #[hotpath::skip]
    const fn is_initial(self) -> bool {
        self.high == 0 && self.low == 0 && self.files == 0
    }

    #[hotpath::skip]
    const fn from_parse_offset(offset: ParseOffset) -> Self {
        Self {
            high: offset.byte_offset,
            low: offset.mtime,
            files: offset.file_id,
        }
    }

    #[hotpath::skip]
    const fn into_parse_offset(self) -> ParseOffset {
        ParseOffset {
            byte_offset: self.high,
            mtime: self.low,
            file_id: self.files,
        }
    }

    fn observe(&mut self, digest: [u8; 32]) -> TranscriptIngestResult<()> {
        let mut high = [0u8; 8];
        let mut low = [0u8; 8];
        high.copy_from_slice(&digest[..8]);
        low.copy_from_slice(&digest[8..16]);
        self.high ^= u64::from_be_bytes(high);
        self.low ^= u64::from_be_bytes(low);
        self.files = self.files.checked_add(1).ok_or(
            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                detail: "Codex corpus file count overflowed",
            },
        )?;
        Ok(())
    }
}

/// Outcome of one recent-first Codex discovery pass.
#[derive(Clone, Debug)]
pub struct CodexDiscoveryPass {
    pub report: FileDiscoveryReport,
    pub(crate) next_frontier: CodexDiscoveryFrontier,
    selected_sources: Vec<CodexFileIdentity>,
    _shared_page_pin: Option<std::sync::Arc<SharedJsonlPathPin>>,
}

fn retained_scan_step(
    source: &CodexSource,
    state: &mut CodexDiscoveryState,
    bounds: TranscriptDiscoveryBounds,
    frontier: CodexDiscoveryFrontier,
) -> TranscriptIngestResult<CodexDiscoveryPass> {
    let scan = state
        .scan
        .as_mut()
        .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
            detail: "retained Codex scan state is missing",
        })?;
    let work_limit = bounds.max_files.max(1);
    // The structural walk and the candidate walk are distinct bounded budgets.
    // Charging both against one counter lets a deep-but-small tree spend the
    // whole pass discovering its own bucket layout, leaving nothing to retain
    // candidates with — a corpus far under `max_files` then reports truncation
    // forever. Each phase is bounded independently.
    // Reaching one candidate can require walking every component of its dated
    // bucket path. Scale structural work with the requested candidate slice
    // and the accepted depth, rather than raising every call to the global
    // retained-directory ceiling.
    let directory_work_limit =
        work_limit.saturating_mul(usize::from(MAX_SCAN_DEPTH).saturating_add(2));
    let mut directory_work = 0usize;
    let mut file_work = 0usize;
    let mut paths = Vec::new();
    let mut selected_sources = Vec::new();
    let mut bytes_charged = 0u64;
    let metadata_charge = std::mem::size_of::<std::fs::Metadata>() as u64;
    let mut discovery_limit = None;

    loop {
        match &mut scan.phase {
            CodexScanPhase::Directories { queued, current } => {
                if directory_work >= directory_work_limit {
                    break;
                }
                if !scan.directories.is_empty()
                    && (scan.directories.len().saturating_add(queued.len()) >= MAX_BUCKET_DIRS
                        || scan.directory_bytes >= bounds.max_discovery_bytes)
                {
                    let resume_directories = CodexDirectoryResume {
                        queued: std::mem::take(queued),
                        current: current.take(),
                    };
                    scan.phase = CodexScanPhase::Files {
                        next_directory: 0,
                        current: None,
                        deferred: None,
                        resume_directories: Some(resume_directories),
                    };
                    continue;
                }
                if current.is_none() {
                    let Some((path, depth, root_order, retained_charge)) = queued.pop_front()
                    else {
                        scan.directories.sort_unstable_by(|left, right| {
                            left.root_order.cmp(&right.root_order).then_with(|| {
                                if left.root_order == 0 {
                                    right.path.cmp(&left.path)
                                } else {
                                    left.path.cmp(&right.path)
                                }
                            })
                        });
                        scan.phase = CodexScanPhase::Files {
                            next_directory: 0,
                            current: None,
                            deferred: None,
                            resume_directories: None,
                        };
                        continue;
                    };
                    directory_work += 1;
                    let charge = if retained_charge == 0 {
                        candidate_charge(&path, metadata_charge)?
                    } else {
                        retained_charge
                    };
                    if path_byte_len(&path) > bounds.max_path_bytes
                        || metadata_charge > bounds.max_metadata_bytes
                    {
                        // Unrepresentable under these bounds: the directory can
                        // never be retained, so the sweep is genuinely partial.
                        scan.complete = false;
                        discovery_limit = Some(FileDiscoveryLimit::DiscoveryBytes);
                        continue;
                    }
                    if retained_charge == 0
                        && scan
                            .directory_bytes
                            .checked_add(charge)
                            .is_none_or(|total| total > bounds.max_discovery_bytes)
                    {
                        // Merely out of retention bytes for this chunk. Dropping
                        // the directory here permanently poisoned `complete`, so
                        // a tight byte budget could cover every file and still
                        // never earn the sweep-complete watermark. Defer it the
                        // same way an over-budget child directory is deferred:
                        // drain what is retained, then resume from this entry.
                        if scan.directories.is_empty() {
                            return Err(TranscriptIngestError::ScanIo {
                                operation: "retain Codex transcript directory authority",
                                path,
                                source: std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "Codex discovery byte budget cannot retain one directory",
                                ),
                            });
                        }
                        queued.push_front((path, depth, root_order, 0));
                        let resume_directories = CodexDirectoryResume {
                            queued: std::mem::take(queued),
                            current: current.take(),
                        };
                        scan.phase = CodexScanPhase::Files {
                            next_directory: 0,
                            current: None,
                            deferred: None,
                            resume_directories: Some(resume_directories),
                        };
                        continue;
                    }
                    if retained_charge == 0 {
                        scan.directory_bytes = scan.directory_bytes.checked_add(charge).ok_or(
                            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                detail: "Codex retained-directory charge overflowed",
                            },
                        )?;
                    }
                    let identity = codex_directory_identity(&path)?;
                    scan.directories.push(CodexDirectoryIdentity {
                        path: path.clone(),
                        root_order,
                        identity,
                    });
                    if identity.is_none() {
                        continue;
                    }
                    let listed = std::fs::read_dir(&path).map_err(|source| {
                        TranscriptIngestError::ScanIo {
                            operation: "enumerate Codex transcript directories",
                            path: path.clone(),
                            source,
                        }
                    })?;
                    *current = Some((path, depth, root_order, listed));
                    continue;
                }
                let (dir, depth, root_order, listed) = current.as_mut().ok_or(
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "retained Codex directory iterator disappeared",
                    },
                )?;
                match listed.next() {
                    Some(entry) => {
                        directory_work += 1;
                        let entry = entry.map_err(|source| TranscriptIngestError::ScanIo {
                            operation: "read Codex transcript directory entry",
                            path: dir.clone(),
                            source,
                        })?;
                        let file_type =
                            entry
                                .file_type()
                                .map_err(|source| TranscriptIngestError::ScanIo {
                                    operation: "read Codex transcript entry type",
                                    path: entry.path(),
                                    source,
                                })?;
                        if file_type.is_dir() && !file_type.is_symlink() {
                            if *depth >= MAX_SCAN_DEPTH {
                                return Err(TranscriptIngestError::ScanIo {
                                    operation: "traverse Codex transcript directory depth",
                                    path: entry.path(),
                                    source: std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        "Codex transcript directory exceeds maximum depth",
                                    ),
                                });
                            } else {
                                let child = entry.path();
                                let charge = candidate_charge(&child, metadata_charge)?;
                                if path_byte_len(&child) > bounds.max_path_bytes
                                    || metadata_charge > bounds.max_metadata_bytes
                                {
                                    return Err(TranscriptIngestError::ScanIo {
                                        operation: "retain Codex transcript directory authority",
                                        path: child,
                                        source: std::io::Error::new(
                                            std::io::ErrorKind::InvalidInput,
                                            "Codex discovery bounds cannot represent directory",
                                        ),
                                    });
                                }
                                let charged = scan.directory_bytes.checked_add(charge).ok_or(
                                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                        detail: "Codex retained-directory charge overflowed",
                                    },
                                )?;
                                if charged > bounds.max_discovery_bytes {
                                    if scan.directories.is_empty() {
                                        return Err(TranscriptIngestError::ScanIo {
                                            operation: "retain Codex transcript directory authority",
                                            path: child,
                                            source: std::io::Error::new(
                                                std::io::ErrorKind::InvalidInput,
                                                "Codex discovery byte budget cannot retain one directory",
                                            ),
                                        });
                                    }
                                    queued.push_front((
                                        child,
                                        depth.checked_add(1).ok_or(
                                            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                                detail: "Codex transcript directory depth overflowed",
                                            },
                                        )?,
                                        *root_order,
                                        0,
                                    ));
                                    let resume_directories = CodexDirectoryResume {
                                        queued: std::mem::take(queued),
                                        current: current.take(),
                                    };
                                    scan.phase = CodexScanPhase::Files {
                                        next_directory: 0,
                                        current: None,
                                        deferred: None,
                                        resume_directories: Some(resume_directories),
                                    };
                                    continue;
                                }
                                scan.directory_bytes = charged;
                                queued.push_front((
                                    child,
                                    depth.checked_add(1).ok_or(
                                        TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                            detail: "Codex transcript directory depth overflowed",
                                        },
                                    )?,
                                    *root_order,
                                    charge,
                                ));
                            }
                        }
                    }
                    None => *current = None,
                }
            }
            CodexScanPhase::Files {
                next_directory,
                current,
                deferred,
                resume_directories,
            } => {
                // Validation retains its iterator and epoch across calls just
                // like an emit sweep. It emits no candidates, but must still
                // obey the per-call work budget so a large Codex history cannot
                // monopolize one scheduler turn.
                if paths.len() >= bounds.max_files || file_work >= work_limit {
                    break;
                }
                let candidate = if let Some(candidate) = deferred.take() {
                    Some(*candidate)
                } else {
                    loop {
                        if current.is_none() {
                            let Some(directory) = scan.directories.get(*next_directory) else {
                                break None;
                            };
                            *next_directory += 1;
                            if directory.identity.is_none() {
                                continue;
                            }
                            let listed = std::fs::read_dir(&directory.path).map_err(|source| {
                                TranscriptIngestError::ScanIo {
                                    operation: "enumerate Codex transcript bucket",
                                    path: directory.path.clone(),
                                    source,
                                }
                            })?;
                            *current = Some((*next_directory - 1, listed));
                        }
                        let (directory_index, listed) = current.as_mut().ok_or(
                            TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                                detail: "retained Codex file iterator disappeared",
                            },
                        )?;
                        if file_work >= work_limit {
                            break None;
                        }
                        match listed.next() {
                            Some(entry) => {
                                file_work += 1;
                                let entry =
                                    entry.map_err(|source| TranscriptIngestError::ScanIo {
                                        operation: "read Codex transcript directory entry",
                                        path: scan.directories[*directory_index].path.clone(),
                                        source,
                                    })?;
                                let path = entry.path();
                                if path.extension().and_then(|value| value.to_str())
                                    != Some("jsonl")
                                {
                                    continue;
                                }
                                let metadata = std::fs::metadata(&path).map_err(|source| {
                                    TranscriptIngestError::ScanIo {
                                        operation: "stat Codex transcript candidate",
                                        path: path.clone(),
                                        source,
                                    }
                                })?;
                                if metadata.is_file() {
                                    break Some((path, metadata));
                                }
                            }
                            None => *current = None,
                        }
                    }
                };
                let Some((path, metadata)) = candidate else {
                    if *next_directory >= scan.directories.len() && current.is_none() {
                        if let Some(resume) = resume_directories.take() {
                            scan.directories.clear();
                            scan.directory_bytes = 0;
                            scan.phase = CodexScanPhase::Directories {
                                queued: resume.queued,
                                current: resume.current,
                            };
                            continue;
                        }
                        if scan.validation && scan.epoch != frontier.epoch {
                            // Report the newest retained window rather than the
                            // readdir-order prefix: `active_files` is the
                            // bounded top-`max_files` set by path, and Codex
                            // rollout names are timestamp-ordered, so this is
                            // the recent-first slice within a bucket as well as
                            // across buckets.
                            selected_sources = scan
                                .active_files
                                .clone()
                                .into_sorted_vec()
                                .into_iter()
                                .map(|Reverse(file)| file)
                                .collect();
                            paths = selected_sources
                                .iter()
                                .map(|file| file.path.clone())
                                .collect();
                            let report = FileDiscoveryReport {
                                paths,
                                truncated: Some(FileDiscoveryLimit::FileCount),
                                skipped_oversized_entries: scan.skipped_oversized_entries,
                                bytes_charged,
                                files_considered: scan.files_considered,
                            };
                            let observed = scan.epoch;
                            *scan = CodexRetainedScan::new(source, false);
                            return Ok(CodexDiscoveryPass {
                                report,
                                // The sweep observed the whole corpus, so the
                                // observed epoch is the authority now. Keeping
                                // the contradicted incoming epoch left the
                                // frontier unable to advance across a
                                // same-path/same-size replacement.
                                next_frontier: CodexDiscoveryFrontier::in_progress(observed),
                                selected_sources,
                                _shared_page_pin: None,
                            });
                        }
                        // A frontier that already claimed completion at epoch E
                        // is contradicted by a sweep that observes E' != E: the
                        // corpus grew or was rewritten under the watermark, and
                        // this pass emitted at most `max_files` of it. Keep
                        // catch-up scheduled rather than re-claiming completion
                        // on the spot; the next pass, entering in-progress, may
                        // claim it once a sweep observes no further change.
                        let grew_under_watermark =
                            frontier.is_complete() && scan.epoch != frontier.epoch;
                        let next_frontier = if scan.complete && !grew_under_watermark {
                            CodexDiscoveryFrontier::complete(scan.epoch)
                        } else {
                            CodexDiscoveryFrontier::in_progress(scan.epoch)
                        };
                        let idle = CodexIdleProbe {
                            directories: scan.directories.clone(),
                            active_files: scan
                                .active_files
                                .iter()
                                .map(|Reverse(file)| file.clone())
                                .collect(),
                            next: 0,
                            epoch: scan.epoch,
                            complete: scan.complete,
                            completed_probe_cycles: 0,
                        };
                        let skipped_oversized_entries = scan.skipped_oversized_entries;
                        let files_considered = scan.files_considered;
                        let pass = CodexDiscoveryPass {
                            report: FileDiscoveryReport {
                                paths,
                                truncated: (!next_frontier.is_complete()).then_some(
                                    discovery_limit.unwrap_or(FileDiscoveryLimit::FileCount),
                                ),
                                skipped_oversized_entries,
                                bytes_charged,
                                files_considered,
                            },
                            next_frontier,
                            selected_sources,
                            _shared_page_pin: None,
                        };
                        state.idle = Some(idle);
                        state.scan = None;
                        return Ok(pass);
                    }
                    break;
                };
                scan.files_considered = scan.files_considered.checked_add(1).ok_or(
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "Codex considered-file count overflowed",
                    },
                )?;
                let path_bytes = path_byte_len(&path);
                if path_bytes > bounds.max_path_bytes || metadata_charge > bounds.max_metadata_bytes
                {
                    scan.complete = false;
                    scan.skipped_oversized_entries = scan
                        .skipped_oversized_entries
                        .checked_add(1)
                        .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                            detail: "Codex oversized-entry count overflowed",
                        })?;
                    discovery_limit = Some(if path_bytes > bounds.max_path_bytes {
                        FileDiscoveryLimit::PathBytes
                    } else {
                        FileDiscoveryLimit::MetadataBytes
                    });
                    continue;
                }
                let charge = candidate_charge(&path, metadata_charge)?;
                let charged = bytes_charged.checked_add(charge).ok_or(
                    TranscriptIngestError::InvalidCodexDiscoveryFrontier {
                        detail: "selected discovery charge overflowed",
                    },
                )?;
                if !scan.validation && charged > bounds.max_discovery_bytes {
                    *deferred = Some(Box::new((path, metadata)));
                    discovery_limit = Some(FileDiscoveryLimit::DiscoveryBytes);
                    break;
                }
                let identity = codex_corpus_identity(&path, &metadata)?;
                scan.epoch.observe(identity)?;
                retain_active_file(
                    &mut scan.active_files,
                    CodexFileIdentity {
                        path: path.clone(),
                        identity,
                    },
                    bounds.max_files,
                );
                if !scan.validation {
                    bytes_charged = charged;
                    paths.push(path.clone());
                    selected_sources.push(CodexFileIdentity { path, identity });
                }
            }
        }
    }

    Ok(CodexDiscoveryPass {
        report: FileDiscoveryReport {
            paths,
            truncated: Some(discovery_limit.unwrap_or(FileDiscoveryLimit::FileCount)),
            skipped_oversized_entries: scan.skipped_oversized_entries,
            bytes_charged,
            files_considered: scan.files_considered,
        },
        // This pass ran out of budget mid-sweep. Echoing a Complete frontier
        // back would let a truncated pass keep a sweep-complete watermark the
        // sweep has not earned, so coverage stays in-progress until a pass
        // actually finishes the walk.
        next_frontier: frontier.for_coverage(false),
        selected_sources,
        _shared_page_pin: None,
    })
}

fn retain_active_file(
    files: &mut BinaryHeap<Reverse<CodexFileIdentity>>,
    candidate: CodexFileIdentity,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    if files.len() < limit {
        files.push(Reverse(candidate));
        return;
    }
    if files
        .peek()
        .is_some_and(|Reverse(oldest)| candidate.path > oldest.path)
    {
        files.pop();
        files.push(Reverse(candidate));
    }
}

fn codex_directory_identity(path: &Path) -> TranscriptIngestResult<Option<[u8; 32]>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(TranscriptIngestError::ScanIo {
                operation: "stat Codex transcript directory",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(TranscriptIngestError::ScanIo {
            operation: "open Codex transcript root as a directory",
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "Codex transcript root is not a directory",
            ),
        });
    }
    Ok(Some(codex_corpus_identity(path, &metadata)?))
}

fn candidate_charge(path: &Path, metadata_charge: u64) -> TranscriptIngestResult<u64> {
    u64::try_from(path_byte_len(path))
        .map_err(|_| TranscriptIngestError::InvalidCodexDiscoveryFrontier {
            detail: "candidate path size exceeds discovery accounting range",
        })?
        .checked_add(metadata_charge)
        .ok_or(TranscriptIngestError::InvalidCodexDiscoveryFrontier {
            detail: "candidate discovery charge overflowed",
        })
}

fn codex_corpus_identity(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> TranscriptIngestResult<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-codex-discovery-identity-v2");
    hash_path(&mut hasher, path);
    hasher.update(metadata.len().to_le_bytes());
    #[cfg(unix)]
    {
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.ctime().to_le_bytes());
        hasher.update(metadata.ctime_nsec().to_le_bytes());
        hasher.update(metadata.mtime().to_le_bytes());
        hasher.update(metadata.mtime_nsec().to_le_bytes());
    }
    #[cfg(windows)]
    {
        hasher.update(metadata.file_attributes().to_le_bytes());
        hasher.update(metadata.creation_time().to_le_bytes());
        hasher.update(metadata.last_write_time().to_le_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        hash_system_time(
            &mut hasher,
            path,
            "read Codex transcript modification time",
            metadata.modified(),
        )?;
        hash_system_time(
            &mut hasher,
            path,
            "read Codex transcript creation time",
            metadata.created(),
        )?;
    }
    Ok(hasher.finalize().into())
}

#[cfg(not(any(unix, windows)))]
fn hash_system_time(
    hasher: &mut Sha256,
    path: &Path,
    operation: &'static str,
    time: std::io::Result<std::time::SystemTime>,
) -> TranscriptIngestResult<()> {
    let duration = time
        .and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .map_err(|source| TranscriptIngestError::ScanIo {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
    hasher.update(duration.as_nanos().to_le_bytes());
    Ok(())
}

fn path_byte_len(path: &Path) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .count()
            .saturating_mul(std::mem::size_of::<u16>())
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.as_os_str().len()
    }
}

fn hash_path(hasher: &mut Sha256, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update([0]);
}

impl TranscriptSource for CodexSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk())
            .paths
    }

    fn discover_transcript_paths(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        match self
            .discover_transcript_paths_with_frontier(bounds, CodexDiscoveryFrontier::initial())
        {
            Ok(pass) => pass.report,
            Err(error) => {
                tracing::warn!(error = %error, "Codex transcript discovery failed");
                FileDiscoveryReport {
                    paths: Vec::new(),
                    truncated: Some(FileDiscoveryLimit::FileCount),
                    skipped_oversized_entries: 0,
                    bytes_charged: 0,
                    files_considered: 0,
                }
            }
        }
    }

    #[hotpath::measure(label = "sessions.hosts.codex.parse")]
    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        // `session_meta` (line 1) is authoritative for session identity and the
        // initial cwd. Later context records can move one rollout between scopes.
        let meta = session_meta(path)?;
        if self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .is_some_and(|session_id| session_id != meta.session_id)
        {
            return None;
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        // Collapses identical consecutive goal states within this parse pass:
        // `thread_goal_updated` fires on every token/time tick, so only an
        // objective- or status-change opens a new `goal` row.
        let mut last_goal_key: Option<(String, Option<String>)> = None;
        let mut structured = events::CodexStructuredState::new();
        // Namespacing follows the stored cursor generation, so every batch of a
        // rewritten file is namespaced; prior-context recovery follows this
        // batch's own resume point, which is zero only at the file head.
        let namespace_replacement = new.replacement_generation;
        let mut context_state = if new.start_offset > 0 {
            CodexContextState::scan_prior(path, new.start_offset, &meta)
        } else {
            CodexContextState::from_meta(&meta)
        };
        let scope_matcher = TranscriptScopeMatcher::for_scope_cached(
            project_root,
            self.user_scope
                .as_ref()
                .map(|scope| scope.registered_roots.as_slice()),
            &self.project_matchers,
        );
        let mut last_in_scope_cwd = None;
        let mut last_in_scope_git = None;
        let push_annotated = |messages: &mut Vec<_>,
                              mut message,
                              cwd: Option<&Path>,
                              git: Option<&serde_json::Value>| {
            context::annotate_message(&mut message, cwd, git, &self.project_matchers);
            if let Some(previous) = messages.last_mut()
                && let (
                    Some((previous_goal, previous_is_current)),
                    Some((message_goal, message_is_current)),
                ) = (
                    goal_context_dedup_projection(previous),
                    goal_context_dedup_projection(&message),
                )
                && previous_goal == message_goal
                && (previous_is_current
                    || message_is_current
                    || previous.message_id == message.message_id)
            {
                if message_is_current && !previous_is_current {
                    if with_paired_response_goal(&mut message, &previous.message_id) {
                        *previous = message;
                        return;
                    }
                } else {
                    return;
                }
            }
            messages.push(message);
        };
        for line in &new.lines {
            let is_context_record = context_state.observe_context_record(&line.value, path, &meta);
            // `Unknown` means a bounded git timeout left this record's scope
            // undecided: abort before any cursor can be persisted so the same
            // bytes are re-parsed (and re-resolved) on the next scan pass.
            let in_scope = match scope_matcher.membership(context_state.cwd.as_deref()) {
                ProjectMembership::Match => true,
                ProjectMembership::NoMatch => false,
                ProjectMembership::Unknown => return None,
            };
            if !in_scope {
                if compacted_summary_from_line(
                    &line.value,
                    &meta,
                    context_state.model.as_deref(),
                    path,
                    line.offset,
                    context_state.compaction_depth + 1,
                )
                .is_some()
                {
                    context_state.compaction_depth += 1;
                }
                continue;
            }
            last_in_scope_cwd.clone_from(&context_state.cwd);
            last_in_scope_git.clone_from(&context_state.git);
            let cwd = context_state.cwd.as_deref();
            let git = context_state.git.as_ref();
            // Non-consuming: harvest session-level policy/effort/rate-limit
            // summary before the line is routed to its owning handler below.
            structured.observe_summary(&line.value);
            if is_context_record {
                continue;
            }
            if let Some(rows) = structured.event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                for message in rows {
                    push_annotated(&mut messages, message, cwd, git);
                }
                continue;
            }
            if let Some(event) = codex_goal_event_from_line(&line.value) {
                let key = event.dedup_key();
                if last_goal_key.as_ref() == Some(&key) {
                    continue;
                }
                last_goal_key = Some(key);
                push_annotated(
                    &mut messages,
                    goal_event_message(
                        &meta,
                        context_state.model.as_deref(),
                        path,
                        line.offset,
                        timestamp_from_record(&line.value),
                        &event,
                    ),
                    cwd,
                    git,
                );
                continue;
            }
            if let Some(message) = response_item_goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = response_item_tool_event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = compacted_summary_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
                context_state.compaction_depth + 1,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                context_state.compaction_depth += 1;
                continue;
            }
            if let Some(message) = goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
                continue;
            }
            if let Some(message) = message_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                push_annotated(&mut messages, message, cwd, git);
            }
        }
        // Emit any `exec_command` calls whose paired output never arrived in
        // this pass so the tool call is not silently dropped.
        for message in structured.flush_pending(&meta, path) {
            push_annotated(
                &mut messages,
                message,
                last_in_scope_cwd.as_deref(),
                last_in_scope_git.as_ref(),
            );
        }

        // A truncate-and-rewrite can reuse every byte offset from the previous
        // file generation. Legacy projection keys are offset-based, so keep
        // replacement rows distinct instead of overwriting retained history.
        if namespace_replacement {
            namespace_replacement_message_ids(&mut messages, new.new_cursor.file_id);
        }

        let project = self.user_scope.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            // The summary is session-wide and may include evidence observed
            // after Codex changed cwd into a registered project. User scope
            // stores only the filtered message rows, never that mixed summary.
            metadata_json: context::session_metadata_json(
                &meta,
                self.user_scope.is_none().then_some(&structured.summary),
                &self.project_matchers,
            ),
            parent_session_id: meta.parent_session_id.clone(),
            is_subagent: meta.is_subagent,
            agent_id: meta.agent_id.clone(),
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        preflight_and_parse_new(PROVIDER, path, prev, max_new_bytes, || {
            self.parse_new(path, prev, project_root, max_new_bytes)
        })
    }
}
