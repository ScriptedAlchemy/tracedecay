use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{ObservationOrderingDomainV1, ObservationSourceCursorV1};
use tracedecay_private_fs::{ChangeClockReading, ChangeStamp, RewriteWitness};
use tracedecay_runtime_core::db::engine::{Error as EngineError, FromValue, Row, Value};
use tracedecay_runtime_core::native_runtime;
use tracedecay_store::{SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord, SessionRecord};

use crate::runtime::SessionMessageSearchResult;
use tracedecay_lcm::raw::{
    message_body_from_record_row, message_body_record_select_columns, message_record_select_columns,
};
use tracedecay_lcm::retrieval_content::{
    RelatedMessageCopyIdentity, dedupe_related_message_copies, rerank_fetch_limit,
};

use super::super::registered_db::{SessionRegisteredDb, SessionStoreAccess};
use super::super::shared::{durable_project_path_key, path_identity_key};
use super::super::source::{
    HostProviderCoverage, JsonlNativeFileIdentity, jsonl_native_file_identity, jsonl_prefix_digest,
};
use super::search::{
    SESSION_MESSAGE_SEARCH_MAX_FETCH, downrank_inventory_messages,
    interleave_workflow_search_results, session_fts_query,
};
use super::types::{
    SessionActivityRow, SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    UNIX_TIMESTAMP_MILLIS_THRESHOLD,
};

const SESSION_INGEST_HEALTH_PAGE_SIZE: i64 = 512;

/// Largest durable message-id window admitted to one indexed lookup.
pub(crate) const SESSION_MESSAGE_ID_LOOKUP_MAX: usize = 256;

/// The JSON rowset is the outer loop and every value probes the canonical
/// `(provider, message_id)` primary key. Keeping this authority here lets
/// provider ingest batch identity reads without duplicating session schema.
pub(crate) const EXISTING_SESSION_MESSAGE_IDS_SQL: &str = "SELECT messages.message_id
     FROM json_each(?2) AS requested
     CROSS JOIN lcm_raw_messages AS messages
     WHERE requested.type = 'text'
       AND messages.provider = ?1
       AND messages.message_id = requested.value";

// A resumed session can move directories. Only messages without a canonical
// storage location inherit the session path.
const MESSAGE_PROJECT_PATH_SQL: &str = "COALESCE(m.location_path, s.project_path)";

/// Appends the project-scope predicate.
///
/// `project_key` is an opaque authority and stays byte-exact. `project_path`
/// may retain its opened spelling in observation projection or its canonical
/// OS identity in transcript persistence. Scoped reads accept both spellings.
fn push_project_identity_predicate(
    sql: &mut String,
    query_params: &mut Vec<Value>,
    project_selector: &str,
    project_path_sql: &str,
) {
    query_params.push(Value::Text(project_selector.to_owned()));
    let key_parameter = query_params.len();
    query_params.push(Value::Text(durable_project_path_key(project_selector)));
    let path_parameter = query_params.len();
    query_params.push(Value::Text(path_identity_key(project_selector)));
    let opened_parameter = query_params.len();
    let _ = write!(
        sql,
        " AND (s.project_key = ?{key_parameter} OR {project_path_sql} IN (?{path_parameter}, ?{opened_parameter}))"
    );
}

fn session_db_operation_error(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> TraceDecayError {
    TraceDecayError::database_operation(operation, source)
}

fn session_db_operation_message(
    operation: &'static str,
    message: impl Into<String>,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}

/// Newest-first ordering where a missing timestamp ranks after every known
/// timestamp instead of being compared as a fabricated epoch-zero time.
fn descending_timestamp(left: Option<i64>, right: Option<i64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}
pub const SESSION_MESSAGES_AFTER_SQL: &str = "SELECT timestamp, ordinal, kind, tool_names, metadata_json \
                 FROM lcm_raw_messages \
                 WHERE provider = ?1 AND session_id = ?2 \
                   AND timestamp IS NOT NULL AND timestamp >= ?3 \
                 ORDER BY timestamp, ordinal, message_id \
                 LIMIT ?4";

type WorkflowFactRow = (String, Option<String>, Option<String>);

/// Cursors the observation authority committed for one logical source.
#[derive(Clone, Default)]
struct AdmittedSource {
    /// Each file-byte cursor retains the checkpoint proving its consumed prefix.
    file_bytes: Vec<ObservationSourceCursorV1>,
    /// The source is also ordered by something other than file bytes, so its
    /// progress is not a byte offset into the transcript.
    other_ordering: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptProgress {
    /// No cursor proves the file's consumed prefix: never ingested, or
    /// rewritten since its cursor was committed.
    Unadmitted,
    /// An unchanged committed prefix is admitted through this byte offset.
    AdmittedThrough(u64),
    /// Progress is measured in rows or snapshots, not transcript bytes.
    NotByteOrdered,
}

/// One transcript version: a later stat that matches it in full, under a
/// settled change token, proves the bytes are the ones a verdict was read from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TranscriptVersion {
    native_identity: JsonlNativeFileIdentity,
    size: u64,
    change: ChangeStamp,
}

/// The prefix verdict last proven for one transcript, with the version and
/// committed checkpoints it was proven against.
#[derive(Clone)]
struct ProvenTranscriptProgress {
    version: TranscriptVersion,
    checkpoints: Vec<(u64, u64)>,
    progress: TranscriptProgress,
}

/// Verdicts of one store's previous health walk, keyed by transcript path.
type TranscriptProgressMemo = HashMap<String, ProvenTranscriptProgress>;

/// Prefix proofs shared by the leases of one registered store publication.
/// Disposal releases the proofs; reversible retirement keeps the same state.
#[derive(Default)]
pub struct SessionTranscriptProgressProofs {
    partitions: Mutex<HashMap<Option<String>, Arc<TranscriptProgressMemo>>>,
}

impl SessionTranscriptProgressProofs {
    fn snapshot(
        &self,
        provider: &Option<String>,
    ) -> Result<Arc<TranscriptProgressMemo>, TraceDecayError> {
        Ok(self
            .partitions
            .lock()
            .map_err(|_| {
                session_db_operation_message(
                    "snapshot session transcript progress proofs",
                    "session transcript progress proofs lock poisoned",
                )
            })?
            .get(provider)
            .cloned()
            .unwrap_or_default())
    }

    fn replace(
        &self,
        provider: Option<String>,
        memo: TranscriptProgressMemo,
    ) -> Result<(), TraceDecayError> {
        self.partitions
            .lock()
            .map_err(|_| {
                session_db_operation_message(
                    "replace session transcript progress proofs",
                    "session transcript progress proofs lock poisoned",
                )
            })?
            .insert(provider, Arc::new(memo));
        Ok(())
    }
}

struct TranscriptProgressRead {
    metadata: std::fs::Metadata,
    progress: TranscriptProgress,
    /// What a later walk may reuse; `None` when no prefix was verified or the
    /// change token cannot yet prove a later stat saw the same bytes.
    proven: Option<ProvenTranscriptProgress>,
    prefix_bytes_hashed: u64,
}

/// Resolves progress through the same committed prefix digest ingest resumes
/// from. A physical replacement can retain its prefix, while an in-place rewrite
/// can change it without changing the file identity or length.
///
/// A `previous` verdict proven for the same version and checkpoints is reused
/// without reading the prefix again.
fn transcript_progress(
    path: &Path,
    sources: &[AdmittedSource],
    previous: Option<&ProvenTranscriptProgress>,
) -> std::io::Result<Option<TranscriptProgressRead>> {
    let mut cursors = sources
        .iter()
        .flat_map(|source| source.file_bytes.iter())
        .collect::<Vec<_>>();
    cursors.sort_unstable_by_key(|cursor| std::cmp::Reverse(cursor.byte_offset()));
    let sampled_at = ChangeClockReading::now();
    let mut file = if cursors.is_empty() {
        None
    } else {
        match std::fs::File::open(path) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    };
    let metadata = match file
        .as_ref()
        .map_or_else(|| std::fs::metadata(path), std::fs::File::metadata)
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "transcript path is not a regular file",
        ));
    }
    let Some(file) = file.as_mut() else {
        let progress = if sources.iter().any(|source| source.other_ordering) {
            TranscriptProgress::NotByteOrdered
        } else {
            TranscriptProgress::Unadmitted
        };
        return Ok(Some(TranscriptProgressRead {
            metadata,
            progress,
            proven: None,
            prefix_bytes_hashed: 0,
        }));
    };
    let checkpoints = cursors
        .iter()
        .filter_map(|cursor| {
            cursor
                .file_identity()
                .and(cursor.resume_fingerprint())
                .map(|fingerprint| (cursor.byte_offset(), fingerprint))
        })
        .collect::<Vec<_>>();
    let version = jsonl_native_file_identity(file, &metadata)
        .map(|native_identity| TranscriptVersion {
            native_identity,
            size: metadata.len(),
            change: RewriteWitness::native_path_stamp(path, &metadata, sampled_at),
        })
        .filter(|version| version.change.is_settled());
    if let (Some(version), Some(previous)) = (version, previous)
        && previous.version == version
        && previous.checkpoints == checkpoints
    {
        return Ok(Some(TranscriptProgressRead {
            metadata,
            progress: previous.progress,
            proven: Some(previous.clone()),
            prefix_bytes_hashed: 0,
        }));
    }
    let mut progress = TranscriptProgress::Unadmitted;
    let mut prefix_bytes_hashed = 0_u64;
    for &(offset, fingerprint) in &checkpoints {
        if offset > metadata.len() {
            continue;
        }
        let (digest, hashed) = jsonl_prefix_digest(file, offset)?;
        prefix_bytes_hashed = prefix_bytes_hashed.saturating_add(hashed);
        if digest.fingerprint(offset) == fingerprint {
            // Descending offsets make the first valid checkpoint the
            // maximum; smaller candidates need no further prefix reads.
            progress = TranscriptProgress::AdmittedThrough(offset);
            break;
        }
    }
    Ok(Some(TranscriptProgressRead {
        metadata,
        progress,
        proven: version.map(|version| ProvenTranscriptProgress {
            version,
            checkpoints,
            progress,
        }),
        prefix_bytes_hashed,
    }))
}

fn modified_unix_seconds(metadata: &std::fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(seconds).ok()
}

impl<D: SessionRegisteredDb + Sync> SessionStoreAccess<'_, D> {
    pub async fn cursor_session_ingest_health(&self) -> Result<SessionIngestHealth, String> {
        self.session_ingest_health_for_provider(Some("cursor"))
            .await
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.ingest_health",
        level = "trace",
        skip_all,
        fields(prefix_bytes_hashed = tracing::field::Empty)
    )]
    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<SessionIngestHealth, String> {
        let mut health = SessionIngestHealth::default();
        // Store-scale scan interleaved with filesystem prefix validation.
        // Holding one pinned snapshot across the whole walk parks a general
        // reader worker for the entire scan; page with short-held query leases
        // instead. The filesystem side of this health estimate is already read
        // live, so a per-page read boundary matches what the result means.
        //
        // Every page is also declared background: this walks the whole store
        // and runs off maintenance, so it admits against the unreserved slice
        // of the reader lane rather than competing with interactive queries.
        let reader = self.read_connection().background();
        let mut observed_providers = BTreeSet::new();
        let mut provider_rows = reader
            .query(
                "SELECT DISTINCT provider
                 FROM sessions
                 WHERE provider IS NOT NULL AND provider != ''
                   AND (?1 IS NULL OR provider = ?1)
                 ORDER BY provider",
                tracedecay_runtime_core::db::engine::params![provider],
            )
            .await
            .map_err(|error| format!("failed to query session ingest providers: {error}"))?;
        while let Some(row) = provider_rows
            .next()
            .await
            .map_err(|error| format!("failed to read session ingest provider: {error}"))?
        {
            observed_providers.insert(
                row.get::<String>(0)
                    .map_err(|error| format!("failed to decode session provider: {error}"))?,
            );
        }
        drop(provider_rows);
        let mut frontier_rows = reader
            .query(
                "SELECT file_path
                 FROM parse_offsets
                 WHERE file_path LIKE 'host-frontier://%/%'
                 ORDER BY file_path",
                (),
            )
            .await
            .map_err(|error| format!("failed to query host ingest frontiers: {error}"))?;
        while let Some(row) = frontier_rows
            .next()
            .await
            .map_err(|error| format!("failed to read host ingest frontier: {error}"))?
        {
            let key = row
                .get::<String>(0)
                .map_err(|error| format!("failed to decode host ingest frontier: {error}"))?;
            if let Some(provider_name) = key
                .strip_prefix("host-frontier://")
                .and_then(|suffix| suffix.split('/').next())
                .filter(|provider_name| !provider_name.is_empty())
                && provider.is_none_or(|selected| selected == provider_name)
            {
                observed_providers.insert(provider_name.to_owned());
            }
        }
        drop(frontier_rows);
        health.observed_providers = observed_providers.into_iter().collect();
        let mut coverage_rows = reader
            .query(
                "SELECT file_path, byte_offset, file_id
                 FROM parse_offsets
                 WHERE file_path LIKE 'host-coverage://%/v1'
                 ORDER BY file_path",
                (),
            )
            .await
            .map_err(|error| format!("failed to query host ingest coverage: {error}"))?;
        while let Some(row) = coverage_rows
            .next()
            .await
            .map_err(|error| format!("failed to read host ingest coverage: {error}"))?
        {
            let key = row
                .get::<String>(0)
                .map_err(|error| format!("failed to decode host ingest coverage key: {error}"))?;
            let Some(provider_name) = key
                .strip_prefix("host-coverage://")
                .and_then(|suffix| suffix.strip_suffix("/v1"))
                .filter(|provider_name| !provider_name.is_empty())
            else {
                continue;
            };
            if provider.is_some_and(|selected| selected != provider_name) {
                continue;
            }
            let deferred = row.get::<i64>(1).map_err(|error| {
                format!("failed to decode host ingest coverage deferred units: {error}")
            })?;
            let deferred_units = u64::try_from(deferred)
                .map_err(|_| format!("negative deferred units for provider {provider_name}"))?;
            let file_id = row
                .get::<i64>(2)
                .map_err(|error| format!("failed to decode host ingest coverage state: {error}"))?;
            let Ok(file_id) = u64::try_from(file_id) else {
                continue;
            };
            let Some((coverage, reason)) = HostProviderCoverage::from_file_id(file_id) else {
                continue;
            };
            let state = match coverage {
                HostProviderCoverage::Complete => SessionProviderCoverageState::Complete,
                HostProviderCoverage::Partial => SessionProviderCoverageState::Partial,
                HostProviderCoverage::Unavailable => SessionProviderCoverageState::Unavailable,
            };
            health.provider_coverage.push(SessionProviderCoverage {
                provider: provider_name.to_owned(),
                state,
                deferred_units,
                reason,
            });
        }
        drop(coverage_rows);
        let admitted = self.admitted_transcript_sources(provider).await?;
        let provider_partition = provider.map(str::to_owned);
        let proofs = self.transcript_progress_proofs();
        let previous_memo = proofs
            .snapshot(&provider_partition)
            .map_err(|error| error.to_string())?;
        let mut next_memo = TranscriptProgressMemo::new();
        let mut prefix_bytes_hashed = 0_u64;
        let mut after_path = String::new();
        loop {
            let mut rows = reader
                .query(
                    "SELECT transcript_path, json_group_array(json_array(provider, session_id))
                     FROM sessions
                     WHERE (?1 IS NULL OR provider = ?1)
                       AND transcript_path IS NOT NULL
                       AND transcript_path != ''
                       AND transcript_path > ?2
                     GROUP BY transcript_path
                     ORDER BY transcript_path
                     LIMIT ?3",
                    tracedecay_runtime_core::db::engine::params![
                        provider,
                        after_path.as_str(),
                        SESSION_INGEST_HEALTH_PAGE_SIZE
                    ],
                )
                .await
                .map_err(|error| format!("failed to query session ingest health: {error}"))?;
            let mut page = Vec::with_capacity(SESSION_INGEST_HEALTH_PAGE_SIZE as usize);
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| format!("failed to read session ingest health: {error}"))?
            {
                let path = row
                    .get::<String>(0)
                    .map_err(|error| format!("failed to decode transcript path: {error}"))?;
                let sessions_json = row
                    .get::<String>(1)
                    .map_err(|error| format!("failed to decode transcript sessions: {error}"))?;
                let sessions: Vec<(String, String)> = serde_json::from_str(&sessions_json)
                    .map_err(|error| format!("invalid transcript sessions: {error}"))?;
                page.push((path, sessions));
            }
            drop(rows);
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            after_path = page
                .last()
                .map(|(path, _)| path.clone())
                .ok_or_else(|| "session ingest health page unexpectedly empty".to_owned())?;
            let page = page
                .into_iter()
                .map(|(path, sessions)| {
                    let sources = sessions
                        .iter()
                        .filter_map(|session| admitted.get(session).cloned())
                        .collect::<Vec<_>>();
                    (path, sources)
                })
                .collect::<Vec<_>>();
            // Prefix checks can read historical transcripts in full. One
            // owned page leaves the async worker and holds no database lease.
            let page_memo = Arc::clone(&previous_memo);
            let (page_health, page_proven, page_hashed) =
                native_runtime::spawn_owned_blocking(move || {
                    let mut proven_page = Vec::new();
                    let mut hashed_page = 0_u64;
                    for (path, sources) in page {
                        // Opaque non-Unicode locations cannot be resolved from
                        // this display field; missing files remain untracked.
                        let Some(read) = ({
                            let _span =
                                tracing::trace_span!("global_db.registered_sessions.ingest_stat")
                                    .entered();
                            transcript_progress(Path::new(&path), &sources, page_memo.get(&path))
                            .map_err(|error| {
                                format!(
                                    "failed to read transcript ingest progress for {path}: {error}"
                                )
                            })?
                        }) else {
                            continue;
                        };
                        hashed_page = hashed_page.saturating_add(read.prefix_bytes_hashed);
                        let TranscriptProgressRead {
                            metadata,
                            progress,
                            proven,
                            ..
                        } = read;
                        if let Some(proven) = proven {
                            proven_page.push((path, proven));
                        }
                        health.tracked_transcripts = health.tracked_transcripts.saturating_add(1);
                        let pending = match progress {
                            TranscriptProgress::Unadmitted => metadata.len(),
                            TranscriptProgress::AdmittedThrough(offset) => {
                                metadata.len().saturating_sub(offset)
                            }
                            TranscriptProgress::NotByteOrdered => 0,
                        };
                        if pending > 0 {
                            health.pending_transcripts =
                                health.pending_transcripts.saturating_add(1);
                            health.pending_bytes = health.pending_bytes.saturating_add(pending);
                            health.max_transcript_pending_bytes =
                                health.max_transcript_pending_bytes.max(pending);
                        } else if matches!(progress, TranscriptProgress::AdmittedThrough(_))
                            && let Some(modified) = modified_unix_seconds(&metadata)
                        {
                            health.last_ingest_unix = Some(
                                health
                                    .last_ingest_unix
                                    .map_or(modified, |previous| previous.max(modified)),
                            );
                        }
                    }
                    Ok::<_, String>((health, proven_page, hashed_page))
                })
                .await
                .map_err(|error| {
                    format!("session ingest health filesystem task failed: {error}")
                })??;
            health = page_health;
            next_memo.extend(page_proven);
            prefix_bytes_hashed = prefix_bytes_hashed.saturating_add(page_hashed);
            if page_len < SESSION_INGEST_HEALTH_PAGE_SIZE as usize {
                break;
            }
        }
        tracing::Span::current().record("prefix_bytes_hashed", prefix_bytes_hashed);
        proofs
            .replace(provider_partition, next_memo)
            .map_err(|error| error.to_string())?;
        Ok(health)
    }

    /// File-byte progress the observation authority recorded for every source
    /// in this store's scope, keyed by the `(provider, session_id)` its
    /// projected session row carries.
    async fn admitted_transcript_sources(
        &self,
        provider: Option<&str>,
    ) -> Result<HashMap<(String, String), AdmittedSource>, String> {
        let scope = self
            .registered_binding()
            .shard_id
            .scope
            .session_scope()
            .ok_or_else(|| "session ingest health requires a session store".to_owned())?;
        let scope_json = serde_json::to_string(&scope)
            .map_err(|error| format!("failed to encode session store scope: {error}"))?;
        let reader = self.read_connection().background();
        let mut admitted = HashMap::<(String, String), AdmittedSource>::new();
        let mut after_source = String::new();
        loop {
            let mut rows = reader
                .query(
                    "SELECT source_json, cursor_json
                     FROM source_cursors
                     WHERE scope_json = ?1 AND source_json > ?2
                     ORDER BY source_json
                     LIMIT ?3",
                    tracedecay_runtime_core::db::engine::params![
                        scope_json.as_str(),
                        after_source.as_str(),
                        SESSION_INGEST_HEALTH_PAGE_SIZE
                    ],
                )
                .await
                .map_err(|error| format!("failed to query admitted source cursors: {error}"))?;
            let mut page_len = 0_usize;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| format!("failed to read admitted source cursor: {error}"))?
            {
                page_len += 1;
                after_source = row
                    .get::<String>(0)
                    .map_err(|error| format!("failed to decode cursor source: {error}"))?;
                let cursor: ObservationSourceCursorV1 = serde_json::from_str(
                    &row.get::<String>(1)
                        .map_err(|error| format!("failed to decode source cursor: {error}"))?,
                )
                .map_err(|error| format!("invalid source cursor: {error}"))?;
                let source = cursor.source();
                if provider.is_some_and(|selected| selected != source.provider().as_str()) {
                    continue;
                }
                let entry = admitted
                    .entry((
                        source.provider().as_str().to_owned(),
                        source.session_id().as_str().to_owned(),
                    ))
                    .or_default();
                if cursor.ordering_domain() == ObservationOrderingDomainV1::FileBytes {
                    entry.file_bytes.push(cursor);
                } else {
                    entry.other_ordering = true;
                }
            }
            drop(rows);
            if page_len < SESSION_INGEST_HEALTH_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(admitted)
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.exists",
        level = "trace",
        skip_all
    )]
    pub async fn has_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, String> {
        // Single-statement point lookup: a pinned read snapshot would hold a
        // general reader lease across BEGIN/pin/query/ROLLBACK, and this runs
        // once per ingested message. Use one short-held query lease instead.
        let mut rows = self
            .read_connection()
            .query(
                "SELECT EXISTS(
                    SELECT 1 FROM lcm_raw_messages
                    WHERE provider = ?1 AND message_id = ?2
                 )",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| format!("failed to query session message existence: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session message existence: {error}"))?
            .ok_or_else(|| "session message existence returned no row".to_string())?;
        row.get::<i64>(0)
            .map(|exists| exists != 0)
            .map_err(|error| format!("failed to decode session message existence: {error}"))
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.exists_batch",
        level = "trace",
        skip_all
    )]
    pub async fn existing_session_message_ids(
        &self,
        provider: &str,
        message_ids: &[String],
    ) -> Result<Vec<String>, String> {
        if message_ids.is_empty() {
            return Ok(Vec::new());
        }
        if message_ids.len() > SESSION_MESSAGE_ID_LOOKUP_MAX {
            return Err(format!(
                "session message identity batch has {} rows; maximum is {SESSION_MESSAGE_ID_LOOKUP_MAX}",
                message_ids.len()
            ));
        }
        let encoded = serde_json::to_string(message_ids)
            .map_err(|error| format!("failed to encode session message identity batch: {error}"))?;
        let mut rows = self
            .read_connection()
            .query(
                EXISTING_SESSION_MESSAGE_IDS_SQL,
                tracedecay_runtime_core::db::engine::params![provider, encoded],
            )
            .await
            .map_err(|error| format!("failed to query session message identity batch: {error}"))?;
        let mut existing = Vec::with_capacity(message_ids.len());
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session message identity batch: {error}"))?
        {
            existing.push(row.get::<String>(0).map_err(|error| {
                format!("failed to decode session message identity batch: {error}")
            })?);
        }
        Ok(existing)
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.count",
        level = "trace",
        skip_all
    )]
    pub async fn session_message_count(&self) -> Result<i64, String> {
        let mut rows = self
            .read_connection()
            .query("SELECT COUNT(*) FROM lcm_raw_messages", ())
            .await
            .map_err(|error| format!("failed to count session messages: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session message count: {error}"))?
            .ok_or_else(|| "session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|error| format!("failed to decode session message count: {error}"))
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.count_project",
        level = "trace",
        skip_all
    )]
    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        let mut sql = "SELECT COUNT(*)
                 FROM lcm_raw_messages m
                 JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
                 WHERE 1 = 1"
            .to_owned();
        let mut query_params = Vec::with_capacity(2);
        push_project_identity_predicate(
            &mut sql,
            &mut query_params,
            project_key,
            MESSAGE_PROJECT_PATH_SQL,
        );
        let mut rows = self
            .read_connection()
            .query(sql.as_str(), query_params)
            .await
            .map_err(|error| format!("failed to count project session messages: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read project session message count: {error}"))?
            .ok_or_else(|| "project session message count returned no row".to_string())?;
        row.get::<i64>(0)
            .map_err(|error| format!("failed to decode project session message count: {error}"))
    }

    #[tracing::instrument(
        name = "global_db.registered_sessions.after",
        level = "trace",
        skip_all
    )]
    pub async fn session_messages_after(
        &self,
        provider: &str,
        session_id: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<SessionActivityRow>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self
            .read_connection()
            .query(
                SESSION_MESSAGES_AFTER_SQL,
                tracedecay_runtime_core::db::engine::params![
                    provider,
                    session_id,
                    since_ts,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
            )
            .await
            .map_err(|error| format!("failed to query session messages after hint: {error}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session messages after hint: {error}"))?
        {
            // Every column is propagated as a typed read failure rather than
            // masked: `.ok().flatten()` conflates "column is SQL NULL" with
            // "the column could not be read", and `unwrap_or_default()` on the
            // ordinal reports position 0 for a row whose position is unknown,
            // which callers read as the first message of the session.
            let column = |index: i32, error: &dyn std::fmt::Display| {
                format!("failed to read session activity column {index}: {error}")
            };
            out.push(SessionActivityRow {
                timestamp: row
                    .get::<Option<i64>>(0)
                    .map_err(|error| column(0, &error))?,
                ordinal: row.get::<i64>(1).map_err(|error| column(1, &error))?,
                kind: row
                    .get::<Option<String>>(2)
                    .map_err(|error| column(2, &error))?,
                tool_names: row
                    .get::<Option<String>>(3)
                    .map_err(|error| column(3, &error))?,
                metadata_json: row
                    .get::<Option<String>>(4)
                    .map_err(|error| column(4, &error))?,
            });
        }
        Ok(out)
    }

    /// Unix seconds of the most recent session activity.
    ///
    /// `Ok(None)` is the truthful "this store holds no timestamped messages";
    /// a failed query or an unreadable timestamp stays an error rather than
    /// masquerading as an idle store.
    #[tracing::instrument(
        name = "global_db.registered_sessions.activity",
        level = "trace",
        skip_all
    )]
    pub async fn latest_session_activity_secs(
        &self,
    ) -> tracedecay_domain::errors::Result<Option<i64>> {
        const OPERATION: &str = "read latest session activity";
        let mut rows = self
            .read_connection()
            .query(
                "WITH latest_seconds AS (
                    SELECT timestamp FROM lcm_raw_messages
                    WHERE timestamp IS NOT NULL
                      AND timestamp < ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 ),
                 latest_millis AS (
                    SELECT timestamp FROM lcm_raw_messages
                    WHERE timestamp >= ?1
                    ORDER BY timestamp DESC
                    LIMIT 1
                 )
                 SELECT timestamp FROM latest_seconds
                 UNION ALL
                 SELECT timestamp FROM latest_millis",
                [UNIX_TIMESTAMP_MILLIS_THRESHOLD],
            )
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?;
        let mut latest: Option<i64> = None;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?
        {
            let timestamp = row
                .get::<i64>(0)
                .map_err(|error| session_db_operation_error(OPERATION, error))?;
            let normalized = if timestamp >= UNIX_TIMESTAMP_MILLIS_THRESHOLD {
                timestamp / 1000
            } else {
                timestamp
            };
            latest = Some(latest.map_or(normalized, |current| current.max(normalized)));
        }
        Ok(latest)
    }

    /// Reads one message by provider and id. `Ok(None)` is truthful absence;
    /// snapshot, query, and row-decode failures stay typed errors.
    #[tracing::instrument(name = "global_db.registered_sessions.get", level = "trace", skip_all)]
    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_domain::errors::Result<Option<SessionMessageRecord>> {
        const OPERATION: &str = "read registered session message";
        let snapshot = self.read_snapshot().await?;
        let sql = format!(
            "SELECT {}
             FROM lcm_raw_messages AS message WHERE provider = ?1 AND message_id = ?2",
            message_body_record_select_columns("message")
        );
        let mut rows = snapshot
            .query(
                &sql,
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?
        else {
            return Ok(None);
        };
        let mut message = row_to_message(&row, 0)
            .map_err(|message| session_db_operation_message(OPERATION, message))?;
        message.text = message_body_from_record_row(&row, 0)
            .map_err(|error| session_db_operation_message(OPERATION, error.to_string()))?;
        Ok(Some(message))
    }

    /// Searches message text for a provider, optionally constrained to one project.
    ///
    /// `Ok(vec![])` is the truthful "nothing matched"; snapshot, query, and
    /// row-decode failures are typed errors instead of an empty result page.
    #[tracing::instrument(
        name = "global_db.registered_sessions.search",
        level = "trace",
        skip_all
    )]
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<Vec<SessionMessageSearchResult>> {
        const OPERATION: &str = "search registered session messages";
        let fts_query = session_fts_query(query);
        if fts_query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let literal_terms = query
            .split_whitespace()
            .filter(|term| term.contains('-'))
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let fetch_limit = rerank_fetch_limit(limit, SESSION_MESSAGE_SEARCH_MAX_FETCH);
        let snapshot = self.read_snapshot().await?;

        let mut sql = format!(
            "SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                {},
                (SELECT p.observation_id
                 FROM observation_projection_provenance p
                 WHERE p.projector_version = ?2
                   AND p.output_provider = m.provider
                   AND p.output_message_id = m.message_id
                 ORDER BY p.output_ordinal
                 LIMIT 1) AS observation_id,
                bm25(lcm_raw_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0) AS rank
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages m ON lcm_raw_messages_fts.rowid = m.store_id
             JOIN sessions s ON s.provider = m.provider AND s.session_id = m.session_id
             WHERE lcm_raw_messages_fts MATCH ?1",
            message_record_select_columns("m")
        );
        let mut query_params = vec![
            Value::Text(fts_query),
            Value::Text(SESSION_MESSAGE_PROJECTOR_VERSION.to_owned()),
            Value::Text(provider.to_owned()),
        ];
        let _ = write!(sql, " AND m.provider = ?{}", query_params.len());
        if let Some(project_key) = project_key {
            push_project_identity_predicate(
                &mut sql,
                &mut query_params,
                project_key,
                MESSAGE_PROJECT_PATH_SQL,
            );
        }
        for term in &literal_terms {
            query_params.push(Value::Text(term.clone()));
            let _ = write!(
                sql,
                " AND instr(lower(m.index_text), ?{}) > 0",
                query_params.len()
            );
        }
        query_params.push(Value::Integer(
            i64::try_from(fetch_limit).unwrap_or(i64::MAX),
        ));
        let _ = write!(
            sql,
            " ORDER BY bm25(lcm_raw_messages_fts, 10.0, 2.0, 1.0, 1.0, 1.0)
              LIMIT ?{}",
            query_params.len()
        );

        let mut transcript_results = Vec::new();
        let mut transcript_observations = BTreeMap::new();
        let mut rows = snapshot
            .query(&sql, query_params)
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?
        {
            let session = row_to_session(&row)
                .map_err(|message| session_db_operation_message(OPERATION, message))?;
            let message = row_to_message(&row, 13)
                .map_err(|message| session_db_operation_message(OPERATION, message))?;
            let score = -row
                .get::<f64>(27)
                .map_err(|error| session_db_operation_error(OPERATION, error))?;
            if let Some(observation_id) = row
                .get::<Option<String>>(26)
                .map_err(|error| session_db_operation_error(OPERATION, error))?
            {
                transcript_observations.insert(observation_id, transcript_results.len());
            }
            transcript_results.push(SessionMessageSearchResult {
                session,
                message,
                score,
            });
        }

        let mut workflow_results: Vec<SessionMessageSearchResult> = Vec::new();
        let mut workflow_observations: BTreeMap<String, usize> = BTreeMap::new();
        let anchored_observations = transcript_observations
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        for (observation_id, fact) in search_workflow_facts(
            &snapshot,
            provider,
            project_key,
            query,
            fetch_limit,
            &anchored_observations,
        )
        .await?
        {
            if let Some(&index) = transcript_observations.get(&observation_id) {
                append_observation_text(
                    &mut transcript_results[index].message.text,
                    &fact.message.text,
                );
                continue;
            }
            if let Some(&index) = workflow_observations.get(&observation_id) {
                append_observation_text(
                    &mut workflow_results[index].message.text,
                    &fact.message.text,
                );
                continue;
            }
            workflow_observations.insert(observation_id, workflow_results.len());
            workflow_results.push(fact);
        }
        let mut results = interleave_workflow_search_results(transcript_results, workflow_results);
        results = dedupe_related_message_copies(results, |result| RelatedMessageCopyIdentity {
            provider: &result.session.provider,
            family_session_id: result
                .session
                .parent_session_id
                .as_deref()
                .unwrap_or(&result.session.session_id),
            session_id: &result.session.session_id,
            is_subagent: result.session.is_subagent,
            content: &result.message.text,
        });
        downrank_inventory_messages(&mut results);
        results.truncate(limit);
        Ok(results)
    }

    /// Lists each session's latest canonical goal state, newest first.
    /// Goals with no native timestamp rank after all timestamped goals
    /// instead of being assigned a fabricated epoch-zero time.
    #[tracing::instrument(
        name = "global_db.registered_sessions.goals",
        level = "trace",
        skip_all
    )]
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<Vec<SessionMessageSearchResult>> {
        const OPERATION: &str = "list recent registered session goals";
        if limit == 0 {
            return Ok(Vec::new());
        }
        let snapshot = self.read_snapshot().await?;
        let mut sql = "WITH ranked_goals AS (
                SELECT w.*,
                       ROW_NUMBER() OVER (
                           PARTITION BY w.provider, w.session_id
                           ORDER BY w.observation_sequence DESC, w.fact_ordinal DESC
                       ) AS goal_rank
                FROM observation_workflow_facts w
                WHERE w.projector_version = ?1
                  AND w.semantic_kind = 'goal'
            )
             SELECT
                s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
                s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
                s.is_subagent, s.agent_id, s.parent_tool_use_id,
                w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
                w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
                w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
                w.source_sequence, w.native_timestamp, w.observation_sequence,
                w.ordering_domain, w.content_json, w.content_text
             FROM ranked_goals w
             JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
             WHERE w.goal_rank = 1"
            .to_owned();
        let mut query_params = vec![Value::Text(SESSION_MESSAGE_PROJECTOR_VERSION.to_owned())];
        if let Some(project_key) = project_key {
            push_project_identity_predicate(
                &mut sql,
                &mut query_params,
                project_key,
                "s.project_path",
            );
        }
        query_params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
        let _ = write!(
            sql,
            " ORDER BY (w.native_timestamp IS NULL) ASC, w.native_timestamp DESC,
                       w.observation_sequence DESC, w.fact_ordinal DESC
              LIMIT ?{}",
            query_params.len()
        );

        let mut results = Vec::new();
        let mut rows = snapshot
            .query(&sql, query_params)
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| session_db_operation_error(OPERATION, error))?
        {
            let session = row_to_session(&row)
                .map_err(|message| session_db_operation_message(OPERATION, message))?;
            let message = row_to_workflow_message(&row, 13)
                .map_err(|message| session_db_operation_message(OPERATION, message))?;
            results.push(SessionMessageSearchResult {
                session,
                message,
                score: 0.0,
            });
        }

        results.sort_by(|left, right| {
            descending_timestamp(left.message.timestamp, right.message.timestamp)
                .then_with(|| right.message.ordinal.cmp(&left.message.ordinal))
                .then_with(|| left.message.message_id.cmp(&right.message.message_id))
        });
        results.truncate(limit);
        Ok(results)
    }

    /// Reads the canonical workflow fact columns used by projection acceptance.
    /// Each row is (kind, status, state).
    #[tracing::instrument(
        name = "global_db.registered_sessions.workflow_facts",
        level = "trace",
        skip_all
    )]
    pub async fn workflow_fact_rows(
        &self,
    ) -> tracedecay_domain::errors::Result<Vec<WorkflowFactRow>> {
        let snapshot = self.read_snapshot().await.map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "begin registered workflow fact snapshot".to_owned(),
                message: error.to_string(),
            }
        })?;
        let mut rows = snapshot
            .query(
                "SELECT semantic_kind, status, state
                 FROM observation_workflow_facts
                 ORDER BY observation_sequence, fact_ordinal",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "query registered workflow facts".to_owned(),
                    message: error.to_string(),
                },
            )?;
        let mut values = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "read registered workflow fact row".to_owned(),
                message: error.to_string(),
            }
        })? {
            values.push((
                row.get(0).map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact kind".to_owned(),
                        message: error.to_string(),
                    }
                })?,
                row.get(1).map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact status".to_owned(),
                        message: error.to_string(),
                    }
                })?,
                row.get(2).map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Database {
                        operation: "decode registered workflow fact state".to_owned(),
                        message: error.to_string(),
                    }
                })?,
            ));
        }
        Ok(values)
    }
}

fn append_observation_text(text: &mut String, fact_text: &str) {
    if fact_text.is_empty() || text.contains(fact_text) {
        return;
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(fact_text);
}

#[tracing::instrument(
    name = "global_db.registered_sessions.workflow_search",
    level = "trace",
    skip_all
)]
async fn search_workflow_facts(
    snapshot: &tracedecay_runtime_core::db::DatabaseEngineReadSnapshot,
    provider: &str,
    project_key: Option<&str>,
    query: &str,
    limit: usize,
    anchored_observations: &[&str],
) -> tracedecay_domain::errors::Result<Vec<(String, SessionMessageSearchResult)>> {
    const OPERATION: &str = "search registered workflow facts";
    let terms = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|character: char| {
                !character.is_alphanumeric() && character != '-' && character != '_'
            })
            .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut sql = "WITH matching_observations AS (
            SELECT
                w.provider,
                w.observation_id,
                MIN(CASE WHEN w.item_order IS NULL THEN 1 ELSE 0 END) AS item_order_missing,
                MIN(w.item_order) AS first_item_order,
                MAX(w.native_timestamp) AS latest_timestamp,
                MAX(w.observation_sequence) AS latest_sequence,
                MIN(w.fact_ordinal) AS first_fact_ordinal
            FROM observation_workflow_facts w
            JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
            WHERE w.projector_version = ?1"
        .to_owned();
    let mut query_params = vec![
        Value::Text(SESSION_MESSAGE_PROJECTOR_VERSION.to_owned()),
        Value::Text(provider.to_owned()),
    ];
    let _ = write!(sql, " AND w.provider = ?{}", query_params.len());
    if let Some(project_key) = project_key {
        push_project_identity_predicate(&mut sql, &mut query_params, project_key, "s.project_path");
    }
    let mut term_predicates = Vec::with_capacity(terms.len());
    for term in terms {
        query_params.push(Value::Text(term));
        term_predicates.push(format!(
            "instr(lower(w.content_text), ?{}) > 0",
            query_params.len()
        ));
    }
    let matching_predicate = term_predicates.join(" OR ");
    let _ = write!(sql, " AND ({matching_predicate})");
    // An observation qualifies on its own only when every term appears in at
    // least one of its facts (master's AND, lifted from fact to observation,
    // the unit a search row now stands for). Observations the transcript
    // search already returned are anchored: any term-matching fact of theirs
    // only enriches that existing row.
    let every_term = term_predicates
        .iter()
        .map(|predicate| format!("SUM({predicate}) > 0"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let _ = write!(
        sql,
        " GROUP BY w.provider, w.observation_id HAVING ({every_term})"
    );
    if !anchored_observations.is_empty() {
        let mut anchored_params = Vec::with_capacity(anchored_observations.len());
        for observation_id in anchored_observations {
            query_params.push(Value::Text((*observation_id).to_owned()));
            anchored_params.push(format!("?{}", query_params.len()));
        }
        let _ = write!(
            sql,
            " OR w.observation_id IN ({})",
            anchored_params.join(", ")
        );
    }
    // The row leads with the observation's fact that matches the most terms,
    // so its metadata names the item the query asked for.
    let matched_terms = term_predicates
        .iter()
        .map(|predicate| format!("({predicate})"))
        .collect::<Vec<_>>()
        .join(" + ");
    query_params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let _ = write!(
        sql,
        " ORDER BY item_order_missing, first_item_order,
                   (latest_timestamp IS NULL) ASC, latest_timestamp DESC,
                   latest_sequence DESC, first_fact_ordinal
          LIMIT ?{}
        )
        SELECT
            s.provider, s.session_id, s.project_key, s.project_path, s.title, s.started_at,
            s.ended_at, s.transcript_path, s.metadata_json, s.parent_session_id,
            s.is_subagent, s.agent_id, s.parent_tool_use_id,
            w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
            w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
            w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
            w.source_sequence, w.native_timestamp, w.observation_sequence,
            w.ordering_domain, w.content_json, w.content_text
        FROM matching_observations matched
        JOIN observation_workflow_facts w
          ON w.provider = matched.provider
         AND w.observation_id = matched.observation_id
         AND w.projector_version = ?1
        JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
        ORDER BY matched.item_order_missing, matched.first_item_order,
                 (matched.latest_timestamp IS NULL) ASC, matched.latest_timestamp DESC,
                 matched.latest_sequence DESC, matched.first_fact_ordinal,
                 ({matched_terms}) DESC,
                 CASE WHEN w.item_order IS NULL THEN 1 ELSE 0 END, w.item_order,
                 (w.native_timestamp IS NULL) ASC, w.native_timestamp DESC,
                 w.fact_ordinal",
        query_params.len()
    );

    let mut rows = snapshot
        .query(&sql, query_params)
        .await
        .map_err(|error| session_db_operation_error(OPERATION, error))?;
    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| session_db_operation_error(OPERATION, error))?
    {
        let session = row_to_session(&row)
            .map_err(|message| session_db_operation_message(OPERATION, message))?;
        let observation_id = row
            .get::<String>(14)
            .map_err(|error| session_db_operation_error(OPERATION, error))?;
        let message = row_to_workflow_message(&row, 13)
            .map_err(|message| session_db_operation_message(OPERATION, message))?;
        results.push((
            observation_id,
            SessionMessageSearchResult {
                session,
                message,
                score: 0.0,
            },
        ));
    }
    Ok(results)
}

fn session_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode session column '{column}': {error}")
}

fn message_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode session message column '{column}': {error}")
}

fn workflow_column_error(column: &str, error: &dyn std::fmt::Display) -> String {
    format!("failed to decode workflow fact column '{column}': {error}")
}

/// Column decode failure. Callers keep their own error text; the column name
/// is the sessions spelling and the source is the engine error.
pub struct SqlColumnError {
    pub column: &'static str,
    pub source: EngineError,
}

fn column<T: FromValue>(row: &Row, index: i32, name: &'static str) -> Result<T, SqlColumnError> {
    row.get(index).map_err(|source| SqlColumnError {
        column: name,
        source,
    })
}

/// `sessions` row in the column order both the session store and the
/// observation projection read.
pub fn session_record_from_row(row: &Row) -> Result<SessionRecord, SqlColumnError> {
    Ok(SessionRecord {
        provider: column(row, 0, "provider")?,
        session_id: column(row, 1, "session_id")?,
        project_key: column(row, 2, "project_key")?,
        project_path: column(row, 3, "project_path")?,
        title: column(row, 4, "title")?,
        started_at: column(row, 5, "started_at")?,
        ended_at: column(row, 6, "ended_at")?,
        transcript_path: column(row, 7, "transcript_path")?,
        metadata_json: column(row, 8, "metadata_json")?,
        parent_session_id: column(row, 9, "parent_session_id")?,
        is_subagent: column::<i64>(row, 10, "is_subagent")? != 0,
        agent_id: column(row, 11, "agent_id")?,
        parent_tool_use_id: column(row, 12, "parent_tool_use_id")?,
    })
}

/// Message row read through one of the `tracedecay_lcm::raw` record column
/// lists, starting at `offset`.
pub fn message_record_from_row(
    row: &Row,
    offset: i32,
) -> Result<SessionMessageRecord, SqlColumnError> {
    Ok(SessionMessageRecord {
        provider: column(row, offset, "provider")?,
        message_id: column(row, offset + 1, "message_id")?,
        session_id: column(row, offset + 2, "session_id")?,
        role: column(row, offset + 3, "role")?,
        timestamp: column(row, offset + 4, "timestamp")?,
        ordinal: column(row, offset + 5, "ordinal")?,
        text: column(row, offset + 6, "text")?,
        kind: column(row, offset + 7, "kind")?,
        model: column(row, offset + 8, "model")?,
        tool_names: column(row, offset + 9, "tool_names")?,
        source_path: column(row, offset + 10, "source_path")?,
        source_offset: column(row, offset + 11, "source_offset")?,
        metadata_json: column(row, offset + 12, "metadata_json")?,
    })
}

fn row_to_session(row: &Row) -> std::result::Result<SessionRecord, String> {
    session_record_from_row(row).map_err(|error| session_column_error(error.column, &error.source))
}

fn row_to_message(row: &Row, offset: i32) -> std::result::Result<SessionMessageRecord, String> {
    message_record_from_row(row, offset)
        .map_err(|error| message_column_error(error.column, &error.source))
}

fn row_to_workflow_message(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> std::result::Result<SessionMessageRecord, String> {
    let provider: String = row
        .get(offset)
        .map_err(|error| workflow_column_error("provider", &error))?;
    let observation_id: String = row
        .get(offset + 1)
        .map_err(|error| workflow_column_error("observation_id", &error))?;
    let fact_ordinal: i64 = row
        .get(offset + 2)
        .map_err(|error| workflow_column_error("fact_ordinal", &error))?;
    let session_id: String = row
        .get(offset + 3)
        .map_err(|error| workflow_column_error("session_id", &error))?;
    let semantic_kind: String = row
        .get(offset + 4)
        .map_err(|error| workflow_column_error("semantic_kind", &error))?;
    let provider_reference: Option<String> = row
        .get(offset + 5)
        .map_err(|error| workflow_column_error("provider_reference", &error))?;
    let item_id: Option<String> = row
        .get(offset + 6)
        .map_err(|error| workflow_column_error("item_id", &error))?;
    let parent_reference: Option<String> = row
        .get(offset + 7)
        .map_err(|error| workflow_column_error("parent_reference", &error))?;
    let list_reference: Option<String> = row
        .get(offset + 8)
        .map_err(|error| workflow_column_error("list_reference", &error))?;
    let state: Option<String> = row
        .get(offset + 9)
        .map_err(|error| workflow_column_error("state", &error))?;
    let status: Option<String> = row
        .get(offset + 10)
        .map_err(|error| workflow_column_error("status", &error))?;
    let item_order: Option<i64> = row
        .get(offset + 11)
        .map_err(|error| workflow_column_error("item_order", &error))?;
    let revision: Option<String> = row
        .get(offset + 12)
        .map_err(|error| workflow_column_error("native_revision", &error))?;
    let event_sequence: Option<i64> = row
        .get(offset + 13)
        .map_err(|error| workflow_column_error("event_sequence", &error))?;
    let source_sequence: Option<i64> = row
        .get(offset + 14)
        .map_err(|error| workflow_column_error("source_sequence", &error))?;
    let native_timestamp: Option<i64> = row
        .get(offset + 15)
        .map_err(|error| workflow_column_error("native_timestamp", &error))?;
    let observation_sequence: i64 = row
        .get(offset + 16)
        .map_err(|error| workflow_column_error("observation_sequence", &error))?;
    let ordering_domain: String = row
        .get(offset + 17)
        .map_err(|error| workflow_column_error("ordering_domain", &error))?;
    let content_json: Option<String> = row
        .get(offset + 18)
        .map_err(|error| workflow_column_error("content_json", &error))?;
    let content_text: String = row
        .get(offset + 19)
        .map_err(|error| workflow_column_error("content_text", &error))?;

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "observation_id".to_owned(),
        JsonValue::String(observation_id.clone()),
    );
    metadata.insert("fact_ordinal".to_owned(), JsonValue::from(fact_ordinal));
    metadata.insert(
        "ordering_domain".to_owned(),
        JsonValue::String(ordering_domain),
    );
    for (key, value) in [
        ("provider_reference", provider_reference),
        ("item_id", item_id),
        ("parent_reference", parent_reference),
        ("list_reference", list_reference),
        ("state", state),
        ("status", status),
        ("revision", revision),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), JsonValue::String(value));
        }
    }
    for (key, value) in [
        ("item_order", item_order),
        ("event_sequence", event_sequence),
        ("source_sequence", source_sequence),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), JsonValue::from(value));
        }
    }
    if let Some(content_json) = content_json {
        let content = serde_json::from_str(&content_json)
            .map_err(|error| workflow_column_error("content_json", &error))?;
        metadata.insert("content".to_owned(), content);
    }

    Ok(SessionMessageRecord {
        provider,
        message_id: format!("workflow/{observation_id}/{fact_ordinal}"),
        session_id,
        role: "system".to_owned(),
        timestamp: native_timestamp,
        ordinal: event_sequence
            .or(source_sequence)
            .unwrap_or(observation_sequence),
        text: content_text,
        kind: Some(semantic_kind),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: Some(JsonValue::Object(metadata).to_string()),
    })
}

#[cfg(all(test, unix))]
mod identity_tests {
    use super::{Value, durable_project_path_key, push_project_identity_predicate};

    #[test]
    fn project_identity_reads_opened_and_canonical_paths_without_aliasing_keys() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let opened = alias.to_str().unwrap();
        let canonical = durable_project_path_key(opened);
        assert_ne!(opened, canonical);
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE sessions (project_key TEXT, project_path TEXT);")
            .unwrap();
        for (key, path) in [
            ("typed-project-a", opened),
            ("typed-project-a", canonical.as_str()),
            (opened, "user"),
            (canonical.as_str(), "user"),
            ("unrelated", "unknown"),
        ] {
            db.execute("INSERT INTO sessions VALUES (?1, ?2)", [key, path])
                .unwrap();
        }
        let mut sql = "SELECT count(*) FROM sessions s WHERE 1 = 1".to_owned();
        let mut params = Vec::new();
        push_project_identity_predicate(&mut sql, &mut params, opened, "s.project_path");
        let params = params.iter().map(|value| match value {
            Value::Text(text) => text.as_str(),
            _ => panic!("project identity parameters must be text"),
        });
        let count: i64 = db
            .query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))
            .unwrap();
        assert_eq!(
            count, 3,
            "both path forms and only the byte-exact opaque key match"
        );
    }
}

#[cfg(test)]
mod transcript_progress_tests {
    use std::sync::{Arc, Barrier};

    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_domain::{
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, SessionId,
    };

    use super::super::super::source::{jsonl_prefix_digest, spin_until_jsonl_change_settled};
    use super::{
        AdmittedSource, SessionTranscriptProgressProofs, TranscriptProgress,
        TranscriptProgressMemo, transcript_progress,
    };

    fn admitted_through(path: &std::path::Path, offset: u64) -> AdmittedSource {
        let mut file = std::fs::File::open(path).unwrap();
        let (digest, _) = jsonl_prefix_digest(&mut file, offset).unwrap();
        let cursor = ObservationSourceCursorV1::for_ordering(
            ObservationSourceIdentityV1::new(SessionId::new("memo-session").unwrap()).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            offset,
        )
        .unwrap()
        .with_resume_checkpoint(1, digest.fingerprint(offset));
        AdmittedSource {
            file_bytes: vec![cursor],
            other_ordering: false,
        }
    }

    #[test]
    fn unchanged_transcript_reuses_its_proven_prefix_until_its_bytes_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let line = format!("{{\"text\":\"{}\"}}\n", "a".repeat(4000));
        std::fs::write(&path, line.repeat(16)).unwrap();
        let length = std::fs::metadata(&path).unwrap().len();
        let sources = [admitted_through(&path, length)];
        assert!(
            spin_until_jsonl_change_settled(&path),
            "this host has no rewrite witness, so no verdict can be reused"
        );

        let first = transcript_progress(&path, &sources, None).unwrap().unwrap();
        assert_eq!(first.progress, TranscriptProgress::AdmittedThrough(length));
        assert_eq!(first.prefix_bytes_hashed, length);
        let proven = first.proven.expect("a settled verdict is reusable");

        let repeat = transcript_progress(&path, &sources, Some(&proven))
            .unwrap()
            .unwrap();
        assert_eq!(repeat.progress, TranscriptProgress::AdmittedThrough(length));
        assert_eq!(
            repeat.prefix_bytes_hashed, 0,
            "an unchanged transcript is not read again"
        );

        let changed_checkpoint = [admitted_through(&path, length / 2)];
        let changed = transcript_progress(&path, &changed_checkpoint, Some(&proven))
            .unwrap()
            .unwrap();
        assert_eq!(
            changed.progress,
            TranscriptProgress::AdmittedThrough(length / 2)
        );
        assert_eq!(changed.prefix_bytes_hashed, length / 2);

        // Same length, same inode, different bytes inside the admitted prefix.
        std::fs::write(&path, line.replace('a', "b").repeat(16)).unwrap();
        let rewritten = transcript_progress(&path, &sources, Some(&proven))
            .unwrap()
            .unwrap();
        assert_eq!(rewritten.progress, TranscriptProgress::Unadmitted);
        assert_eq!(rewritten.prefix_bytes_hashed, length);
    }

    #[test]
    fn progress_proof_partitions_replace_independently_with_immutable_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(&path, b"first\nsecond\n").unwrap();
        assert!(spin_until_jsonl_change_settled(&path));
        let proofs = SessionTranscriptProgressProofs::default();
        let barrier = Barrier::new(2);
        let key = path.to_str().unwrap().to_owned();
        std::thread::scope(|threads| {
            for (provider, offset) in [(None, 6), (Some("claude".to_owned()), 13)] {
                let proofs = &proofs;
                let barrier = &barrier;
                let path = &path;
                let key = &key;
                threads.spawn(move || {
                    let previous = proofs.snapshot(&provider).unwrap();
                    assert!(previous.is_empty());
                    barrier.wait();
                    let read = transcript_progress(
                        path,
                        &[admitted_through(path, offset)],
                        previous.get(key),
                    )
                    .unwrap()
                    .unwrap();
                    assert_eq!(read.prefix_bytes_hashed, offset);
                    proofs
                        .replace(
                            provider,
                            TranscriptProgressMemo::from([(key.clone(), read.proven.unwrap())]),
                        )
                        .unwrap();
                });
            }
        });
        for (provider, offset) in [(None, 6), (Some("claude".to_owned()), 13)] {
            let snapshot = proofs.snapshot(&provider).unwrap();
            let warm = transcript_progress(
                &path,
                &[admitted_through(&path, offset)],
                snapshot.get(&key),
            )
            .unwrap()
            .unwrap();
            assert_eq!(warm.progress, TranscriptProgress::AdmittedThrough(offset));
            assert_eq!(warm.prefix_bytes_hashed, 0);
        }
        let previous = proofs.snapshot(&None).unwrap();
        proofs.replace(None, TranscriptProgressMemo::new()).unwrap();
        assert!(proofs.snapshot(&None).unwrap().is_empty());
        assert!(previous.contains_key(&key));
        assert!(
            proofs
                .snapshot(&Some("claude".to_owned()))
                .unwrap()
                .contains_key(&key)
        );

        let owner = Arc::new(proofs);
        let weak = Arc::downgrade(&owner);
        drop(owner);
        assert!(
            weak.upgrade().is_none(),
            "page snapshots retain no owner state"
        );
        assert!(previous.contains_key(&key));
    }

    #[test]
    fn poisoned_progress_proofs_refuse_snapshot_and_replacement() {
        let proofs = SessionTranscriptProgressProofs::default();
        std::thread::scope(|threads| {
            assert!(
                threads
                    .spawn(|| {
                        let _guard = proofs.partitions.lock().unwrap();
                        panic!("poison transcript progress proof state");
                    })
                    .join()
                    .is_err()
            );
        });
        assert!(matches!(
            proofs.snapshot(&None),
            Err(TraceDecayError::Database { operation, .. })
                if operation == "snapshot session transcript progress proofs"
        ));
        assert!(matches!(
            proofs.replace(None, TranscriptProgressMemo::new()),
            Err(TraceDecayError::Database { operation, .. })
                if operation == "replace session transcript progress proofs"
        ));
    }
}
