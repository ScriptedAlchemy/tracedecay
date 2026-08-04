use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_capture::opencode as opencode_capture;
use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, ProviderId, RetentionClass, SessionId,
};
use tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1;

use crate::admission::HostAdmission;
use crate::observation::{CaptureObservationRequest, ObservationCancellation};
use crate::runtime::snapshot_observation::{
    SnapshotAdmissionRecord, SnapshotAdmissionRunner, SnapshotCaptureOutcome,
};
use crate::runtime::source::{
    TranscriptIngestError, TranscriptIngestResult, canonical_framed_sha256,
};

const PROVIDER: &str = "opencode";
const MAX_ROOTS_PER_PASS: usize = 256;
const MAX_MESSAGES_PER_PASS: usize = 4_096;
const MAX_PARTS_PER_MESSAGE: usize = 256;
const MAX_NATIVE_JSON_BYTES: usize = 1024 * 1024;

pub struct OpenCodeSource {
    database_path: PathBuf,
    roots: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct OpenCodeRecord {
    session_id: String,
    native_record_id: String,
    order: u64,
    payload: Vec<u8>,
}

struct LoadedOpenCode {
    generation: ObservationSourceGenerationV1,
    batches: Vec<(u64, Vec<OpenCodeRecord>)>,
    deferred: bool,
}

impl SnapshotAdmissionRecord for OpenCodeRecord {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    fn order(&self) -> u64 {
        self.order
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }

    fn capture_request(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        cancellation: ObservationCancellation,
    ) -> TranscriptIngestResult<CaptureObservationRequest> {
        let range = ObservationSourceRangeV1::new(self.order, self.order.saturating_add(1))?;
        let native_id = ObservationId::new(&self.native_record_id).map_err(|_| invalid_frame())?;
        let parsed = parse_normalized_observation_record_v1(
            &self.payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                opencode_capture::normalize_observation(
                    &native,
                    &self.session_id,
                    native_id.clone(),
                    range,
                )
            },
        )
        .map_err(|_| TranscriptIngestError::NonDurableRecord {
            provider: PROVIDER,
            offset: range.start(),
            end_offset: range.end(),
            reason: "normalized OpenCode record is not durable",
        })?;
        let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
        let session = SessionId::new(&self.session_id).map_err(|_| invalid_frame())?;
        let source = ObservationSourceIdentityV1::for_provider(provider, session)
            .map_err(|_| invalid_frame())?;
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            generation,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            native_id,
        )?;
        CaptureObservationRequest::new(
            parsed,
            identity,
            expected_cursor,
            RetentionClass::new("transcript.opencode.v1")?,
            cancellation,
        )
        .map_err(|_| invalid_frame())
    }
}

impl OpenCodeSource {
    pub fn new_for_project(project_root: &Path) -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home, vec![project_root.to_path_buf()]))
    }

    pub fn new_for_user(roots: Vec<PathBuf>) -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home, roots))
    }

    pub fn with_home(home: &Path, roots: Vec<PathBuf>) -> Self {
        Self::with_database(opencode_data_dir(home).join("opencode.db"), roots)
    }

    pub fn with_database(database_path: PathBuf, roots: Vec<PathBuf>) -> Self {
        Self {
            database_path,
            roots,
        }
    }

    fn load(&self) -> TranscriptIngestResult<Option<LoadedOpenCode>> {
        if self.roots.is_empty() {
            return Ok(None);
        }
        match std::fs::metadata(&self.database_path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(scan_error(
                    "stat OpenCode database",
                    &self.database_path,
                    error,
                ));
            }
        }
        let connection = tracedecay_rusqlite_runtime::open_immutable_reader(&self.database_path)
            .map_err(|error| scan_error("open immutable database", &self.database_path, error))?;
        let generation = database_generation(&self.database_path)?;
        let mut records = BTreeMap::<String, Vec<OpenCodeRecord>>::new();
        let mut payload_bytes = BTreeMap::<String, u64>::new();
        let mut next_orders = BTreeMap::<String, u64>::new();
        let mut blocked_sessions = BTreeSet::new();
        let mut deferred = self.roots.len() > MAX_ROOTS_PER_PASS;
        let mut remaining = MAX_MESSAGES_PER_PASS.saturating_add(1);
        for root in self.roots.iter().take(MAX_ROOTS_PER_PASS) {
            if remaining == 0 {
                deferred = true;
                break;
            }
            let rows = load_messages_for_root(&connection, root, remaining, &self.database_path)?;
            for row in rows {
                if blocked_sessions.contains(&row.session_id) {
                    continue;
                }
                if records.values().map(Vec::len).sum::<usize>() >= MAX_MESSAGES_PER_PASS {
                    deferred = true;
                    break;
                }
                let order = *next_orders.entry(row.session_id.clone()).or_default();
                let session_id = row.session_id.clone();
                let (record, record_deferred) =
                    load_record(&connection, row, order, &self.database_path)?;
                if record_deferred {
                    deferred = true;
                    blocked_sessions.insert(session_id);
                }
                let Some(record) = record else {
                    continue;
                };
                next_orders.insert(record.session_id.clone(), order.saturating_add(1));
                let session_bytes = payload_bytes.entry(record.session_id.clone()).or_default();
                *session_bytes = session_bytes.saturating_add(record.payload.len() as u64);
                records
                    .entry(record.session_id.clone())
                    .or_default()
                    .push(record);
            }
            remaining = MAX_MESSAGES_PER_PASS
                .saturating_add(1)
                .saturating_sub(records.values().map(Vec::len).sum::<usize>());
        }
        let batches = records
            .into_values()
            .map(|mut session_records| -> TranscriptIngestResult<_> {
                session_records.sort_by(|left, right| {
                    left.order
                        .cmp(&right.order)
                        .then_with(|| left.native_record_id.cmp(&right.native_record_id))
                });
                let session_id = session_records
                    .first()
                    .map(|record| record.session_id.clone())
                    .ok_or_else(invalid_frame)?;
                let bytes = payload_bytes.get(&session_id).copied().ok_or_else(|| {
                    TranscriptIngestError::InvalidSourceIdentity {
                        provider: PROVIDER,
                        path: self.database_path.clone(),
                    }
                })?;
                Ok((bytes, session_records))
            })
            .collect::<TranscriptIngestResult<Vec<_>>>()?;
        Ok(Some(LoadedOpenCode {
            generation,
            batches,
            deferred,
        }))
    }
}

pub async fn capture_opencode_observations(
    facade: &dyn HostAdmission,
    source: &OpenCodeSource,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<SnapshotCaptureOutcome> {
    let Some(loaded) = source.load()? else {
        return Ok(SnapshotCaptureOutcome::default());
    };
    let mut runner = SnapshotAdmissionRunner::new(max_new_bytes);
    if loaded.deferred {
        runner.defer();
    }
    for (input_bytes, records) in loaded.batches {
        runner
            .admit_batch(facade, input_bytes, &scope, cancellation, || {
                Ok(Some((loaded.generation, records)))
            })
            .await?;
    }
    Ok(runner.finish())
}

struct MessageRow {
    id: String,
    session_id: String,
    data: Option<Vec<u8>>,
}

fn load_messages_for_root(
    connection: &Connection,
    root: &Path,
    limit: usize,
    database_path: &Path,
) -> TranscriptIngestResult<Vec<MessageRow>> {
    let mut statement = connection
        .prepare(
            "SELECT m.id, m.session_id, length(m.data),
                    CASE WHEN length(m.data) <= ?1 THEN m.data ELSE NULL END
             FROM message m
             JOIN session s ON s.id = m.session_id
             WHERE s.directory = ?2
             ORDER BY m.session_id, m.time_created, m.id
             LIMIT ?3",
        )
        .map_err(|error| scan_error("prepare message query", database_path, error))?;
    let limit = i64::try_from(limit).map_err(|_| invalid_frame())?;
    let max_bytes = i64::try_from(MAX_NATIVE_JSON_BYTES).map_err(|_| invalid_frame())?;
    let root = root
        .to_str()
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: root.to_path_buf(),
        })?;
    let mut query = statement
        .query(params![max_bytes, root, limit])
        .map_err(|error| scan_error("query messages", database_path, error))?;
    let mut rows = Vec::new();
    while let Some(row) = query
        .next()
        .map_err(|error| scan_error("read message row", database_path, error))?
    {
        let byte_len: i64 = row
            .get(2)
            .map_err(|error| scan_error("decode message length", database_path, error))?;
        let data = sql_bytes(row, 3, database_path, "decode message data")?;
        if byte_len < 0 {
            continue;
        }
        rows.push(MessageRow {
            id: row
                .get(0)
                .map_err(|error| scan_error("decode message id", database_path, error))?,
            session_id: row
                .get(1)
                .map_err(|error| scan_error("decode session id", database_path, error))?,
            data,
        });
    }
    Ok(rows)
}

fn load_record(
    connection: &Connection,
    row: MessageRow,
    order: u64,
    database_path: &Path,
) -> TranscriptIngestResult<(Option<OpenCodeRecord>, bool)> {
    let Some(data) = row.data else {
        return Ok((None, true));
    };
    let Ok(mut message) = serde_json::from_slice::<Value>(&data) else {
        return Ok((None, true));
    };
    let Value::Object(message_fields) = &mut message else {
        return Ok((None, true));
    };
    message_fields
        .entry("id")
        .or_insert_with(|| Value::String(row.id.clone()));
    message_fields
        .entry("sessionID")
        .or_insert_with(|| Value::String(row.session_id.clone()));
    let parts = load_parts(connection, &row.id, database_path)?;
    if parts.deferred {
        return Ok((None, true));
    }
    let payload = serde_json::to_vec(&serde_json::json!({
        "message": message,
        "parts": parts.values,
    }))
    .map_err(|_| invalid_frame())?;
    let native_record_id = stable_native_id(&row.id);
    Ok((
        Some(OpenCodeRecord {
            session_id: row.session_id,
            native_record_id,
            order,
            payload,
        }),
        false,
    ))
}

struct LoadedParts {
    values: Vec<Value>,
    deferred: bool,
}

fn load_parts(
    connection: &Connection,
    message_id: &str,
    database_path: &Path,
) -> TranscriptIngestResult<LoadedParts> {
    let mut statement = connection
        .prepare(
            "SELECT id, length(data),
                    CASE WHEN length(data) <= ?1 THEN data ELSE NULL END
             FROM part
             WHERE message_id = ?2
             ORDER BY id
             LIMIT ?3",
        )
        .map_err(|error| scan_error("prepare part query", database_path, error))?;
    let mut query = statement
        .query(params![
            i64::try_from(MAX_NATIVE_JSON_BYTES).map_err(|_| invalid_frame())?,
            message_id,
            i64::try_from(MAX_PARTS_PER_MESSAGE.saturating_add(1)).map_err(|_| invalid_frame())?
        ])
        .map_err(|error| scan_error("query parts", database_path, error))?;
    let mut values = Vec::new();
    let mut deferred = false;
    while let Some(row) = query
        .next()
        .map_err(|error| scan_error("read part row", database_path, error))?
    {
        if values.len() >= MAX_PARTS_PER_MESSAGE {
            deferred = true;
            break;
        }
        let id: String = row
            .get(0)
            .map_err(|error| scan_error("decode part id", database_path, error))?;
        let data = sql_bytes(row, 2, database_path, "decode part data")?;
        let Some(data) = data else {
            deferred = true;
            continue;
        };
        let Ok(mut value) = serde_json::from_slice::<Value>(&data) else {
            deferred = true;
            continue;
        };
        if let Value::Object(fields) = &mut value {
            fields.entry("id").or_insert(Value::String(id));
        }
        values.push(value);
    }
    Ok(LoadedParts { values, deferred })
}

fn stable_native_id(native: &str) -> String {
    ObservationId::new(native).map_or_else(
        |_| {
            format!(
                "opencode.message.{}",
                canonical_framed_sha256(b"tracedecay.opencode.message.v1", &[native.as_bytes()])
            )
        },
        |_| native.to_owned(),
    )
}

fn sql_bytes(
    row: &Row<'_>,
    index: usize,
    database_path: &Path,
    operation: &'static str,
) -> TranscriptIngestResult<Option<Vec<u8>>> {
    let value = row
        .get_ref(index)
        .map_err(|error| scan_error(operation, database_path, error))?;
    match value {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Ok(Some(bytes.to_vec())),
        _ => Err(scan_error(
            operation,
            database_path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "OpenCode JSON column is not text or blob",
            ),
        )),
    }
}

fn database_generation(path: &Path) -> TranscriptIngestResult<ObservationSourceGenerationV1> {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let bytes: [u8; 8] = digest[..8].try_into().map_err(|_| invalid_frame())?;
    let value = u64::from_be_bytes(bytes).max(1);
    ObservationSourceGenerationV1::new(value).map_err(TranscriptIngestError::from)
}

fn opencode_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/opencode");
    }
    #[cfg(target_os = "windows")]
    {
        return home.join("AppData/Local/opencode");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local/share/opencode")
    }
}

fn scan_error(
    operation: &'static str,
    path: &Path,
    error: impl std::error::Error + Send + Sync + 'static,
) -> TranscriptIngestError {
    TranscriptIngestError::ScanIo {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    }
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;
    use tracedecay_domain::ObservationScopeV1;

    use crate::admission::test_support::MemoryHostAdmission;
    use crate::observation::ObservationCancellation;

    use super::{OpenCodeSource, capture_opencode_observations};

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let other = temp.path().join("other");
        let database = temp.path().join("isolated-opencode.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    directory TEXT NOT NULL
                 );
                 CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    data BLOB NOT NULL
                 );
                 CREATE INDEX message_session_time_created_id_idx
                    ON message(session_id, time_created, id);
                 CREATE TABLE part (
                    id TEXT PRIMARY KEY,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    data BLOB NOT NULL
                 );
                 CREATE INDEX part_message_id_id_idx ON part(message_id, id);",
            )
            .unwrap();
        for (session, directory) in [("ses_project", &project), ("ses_other", &other)] {
            connection
                .execute(
                    "INSERT INTO session(id, directory) VALUES (?1, ?2)",
                    rusqlite::params![session, directory.to_string_lossy()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO message(id, session_id, time_created, data)
                     VALUES (?1, ?2, 1, ?3)",
                    rusqlite::params![
                        format!("msg_{session}"),
                        session,
                        json!({"role": "user", "time": {"created": 1}}).to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO part(id, message_id, session_id, data)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        format!("part_{session}"),
                        format!("msg_{session}"),
                        session,
                        json!({"type": "text", "text": format!("secret-{session}")}).to_string()
                    ],
                )
                .unwrap();
        }
        drop(connection);
        (temp, project, database)
    }

    #[tokio::test]
    async fn immutable_database_read_is_scoped_resumable_and_budgeted() {
        let (_temp, project, database) = fixture();
        let source = OpenCodeSource::with_database(database, vec![project]);
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();

        let deferred = capture_opencode_observations(
            &admission,
            &source,
            ObservationScopeV1::Profile,
            Some(0),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(deferred.deferred_by_byte_cap);
        assert!(admission.observations().is_empty());

        let resumed = capture_opencode_observations(
            &admission,
            &source,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(resumed.stats.messages_upserted, 1);
        assert_eq!(admission.observations().len(), 1);

        let replay = capture_opencode_observations(
            &admission,
            &source,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(replay.stats.messages_upserted, 0);
        assert_eq!(admission.observations().len(), 1);
    }

    #[tokio::test]
    async fn malformed_suffix_defers_without_hiding_committed_prefix() {
        let (_temp, project, database) = fixture();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO message(id, session_id, time_created, data)
                 VALUES ('msg_z_malformed', 'ses_project', 2, '{')",
                (),
            )
            .unwrap();
        drop(connection);
        let source = OpenCodeSource::with_database(database, vec![project]);
        let admission = MemoryHostAdmission::default();

        let outcome = capture_opencode_observations(
            &admission,
            &source,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();

        assert!(outcome.deferred_by_byte_cap);
        assert_eq!(outcome.stats.messages_upserted, 1);
        assert_eq!(admission.observations().len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_database_is_typed_instead_of_empty_success() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let database = temp.path().join("opencode.db");
        symlink(&database, &database).unwrap();
        let source = OpenCodeSource::with_database(database.clone(), vec![temp.path().into()]);

        let error = capture_opencode_observations(
            &MemoryHostAdmission::default(),
            &source,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            crate::runtime::source::TranscriptIngestError::ScanIo {
                operation: "stat OpenCode database",
                path,
                ..
            } if path == database
        ));
    }
}
