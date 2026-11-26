//! Short-lived response-handle records in the registered store.
//!
//! A handle is the truncated SHA-256 of the exact response text. Each row
//! binds that handle to the admitted store-shard namespace that published it
//! and keeps the immutable content, its full digest, its exact byte and
//! character counts, and its creation and expiry together, so one SQLite
//! transaction publishes or renews the whole record. The metadata columns
//! precede the content and a covering index carries `(namespace, expires_at,
//! content_bytes)`, so inventory and expiry selection never read payload
//! pages. Content identity is verified where content is actually returned.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_contracts::{CancellationSignal, Deadline, clock::now_micros};
use tracedecay_domain::errors::{InvalidRequestReason, TraceDecayError};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

use super::{RegisteredGlobalDb, global_db_operation_error};

/// How long a published handle stays retrievable.
pub const RESPONSE_HANDLE_TTL_SECS: i64 = 86_400;

const HANDLE_HEX_CHARS: usize = 24;
const HANDLE_PREFIX: &str = "rh_";
/// Expired rows one cleanup transaction removes, so the writer is held for a
/// bounded batch rather than for every expired payload in the store.
const CLEANUP_BATCH_ROWS: i64 = 256;

const RESPONSE_HANDLE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS response_handles (
        namespace TEXT NOT NULL CHECK(length(namespace) > 0),
        handle TEXT NOT NULL CHECK(length(handle) = 27 AND substr(handle, 1, 3) = 'rh_'),
        content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64),
        content_bytes INTEGER NOT NULL CHECK(content_bytes >= 0),
        content_chars INTEGER NOT NULL CHECK(content_chars >= 0),
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL CHECK(expires_at > created_at),
        content TEXT NOT NULL,
        PRIMARY KEY (namespace, handle)
    ) STRICT;
    CREATE INDEX IF NOT EXISTS idx_response_handles_expiry
        ON response_handles(namespace, expires_at, content_bytes);
";

pub(crate) async fn ensure_response_handle_schema(
    transaction: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    transaction
        .execute_batch(RESPONSE_HANDLE_SCHEMA)
        .await
        .map_err(|error| global_db_operation_error("initialize response-handle schema", error))
}

/// Why the handle store could not answer. An invalid handle is the caller's
/// to correct; a collision or corrupt record is refused rather than served;
/// a store failure is the registered store itself being unavailable.
#[derive(Debug, thiserror::Error)]
pub enum ResponseHandleError {
    #[error(
        "invalid response handle: expected `{HANDLE_PREFIX}` followed by {HANDLE_HEX_CHARS} hex \
         characters copied from a truncated MCP response envelope"
    )]
    InvalidHandle,
    #[error("response handle {handle} already names different content in this store")]
    Collision { handle: String },
    #[error("corrupt response-handle record: {reason}")]
    CorruptRecord { reason: String },
    #[error(transparent)]
    Store(#[from] TraceDecayError),
}

impl From<ResponseHandleError> for TraceDecayError {
    fn from(error: ResponseHandleError) -> Self {
        match error {
            ResponseHandleError::InvalidHandle => Self::InvalidRequest {
                reason: InvalidRequestReason::InvalidResponseHandle,
                message: error.to_string(),
            },
            ResponseHandleError::Collision { .. } | ResponseHandleError::CorruptRecord { .. } => {
                Self::Database {
                    operation: "response handle".to_owned(),
                    message: error.to_string(),
                }
            }
            ResponseHandleError::Store(error) => error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHandleRecord {
    pub handle: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub content: String,
}

impl ResponseHandleRecord {
    pub fn original_chars(&self) -> usize {
        self.content.chars().count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseHandleLookup {
    Found(ResponseHandleRecord),
    Missing,
    Expired { created_at: i64, expires_at: i64 },
}

/// Metadata-only census of one namespace's live and expired rows.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ResponseHandleInventory {
    pub record_count: u64,
    pub total_bytes: u64,
    pub oldest_expires_at: Option<i64>,
    pub newest_expires_at: Option<i64>,
}

pub fn is_valid_response_handle(handle: &str) -> bool {
    let Some(hex) = handle.strip_prefix(HANDLE_PREFIX) else {
        return false;
    };
    hex.len() == HANDLE_HEX_CHARS
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// The canonical content identity of `content`.
pub fn response_handle_for(content: &str) -> String {
    handle_for_digest(&content_digest(content))
}

fn content_digest(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

fn handle_for_digest(digest: &str) -> String {
    format!("{HANDLE_PREFIX}{}", &digest[..HANDLE_HEX_CHARS])
}

fn validate_handle(handle: &str) -> Result<(), ResponseHandleError> {
    if is_valid_response_handle(handle) {
        Ok(())
    } else {
        Err(ResponseHandleError::InvalidHandle)
    }
}

fn store_error(
    operation: &'static str,
) -> impl FnOnce(tracedecay_runtime_core::db::engine::Error) -> ResponseHandleError {
    move |error| ResponseHandleError::Store(global_db_operation_error(operation, error))
}

fn corrupt(reason: impl Into<String>) -> ResponseHandleError {
    ResponseHandleError::CorruptRecord {
        reason: reason.into(),
    }
}

fn integer(row: &Row, column: i32, field: &str) -> Result<i64, ResponseHandleError> {
    row.get::<i64>(column)
        .map_err(|error| corrupt(format!("{field} did not decode: {error}")))
}

fn count(value: usize, field: &str) -> Result<i64, ResponseHandleError> {
    i64::try_from(value).map_err(|_| corrupt(format!("{field} exceeds the stored integer range")))
}

/// What publication found under the handle before it wrote.
enum ExistingRecord {
    Absent,
    Present {
        digest: String,
        created_at: i64,
        expires_at: i64,
        /// The stored text and lengths match the publication, compared inside
        /// SQLite so the payload is not copied out.
        same_content: bool,
    },
}

struct PublicationControl {
    deadline: MonotonicDeadline,
    cancellation: CancellationToken,
    signal: Option<CancellationSignal>,
}

impl PublicationControl {
    fn refusal(&self) -> Option<TraceDecayError> {
        if self.cancellation.is_cancelled()
            || self
                .signal
                .as_ref()
                .is_some_and(CancellationSignal::is_cancelled)
        {
            Some(TraceDecayError::project_route(
                "response_handle_publication_cancelled",
                true,
                "Response-handle publication was cancelled",
            ))
        } else if self.deadline.is_elapsed_at(Instant::now()) {
            Some(TraceDecayError::project_route(
                "response_handle_publication_timed_out",
                true,
                "Response-handle publication exceeded its request deadline",
            ))
        } else {
            None
        }
    }
    async fn cancelled(&self) {
        if let Some(signal) = &self.signal {
            tokio::select! {
                _ = self.cancellation.cancelled() => {},
                _ = signal.cancelled() => {},
            }
        } else {
            self.cancellation.cancelled().await;
        }
    }
}

impl ExactSqlWriteAuthority for PublicationControl {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        match self.refusal() {
            Some(error) => Err(ExactSqlError::AuthorityDenied(error.to_string())),
            None => Ok(()),
        }
    }
}

struct CancelPublicationOnDrop(CancellationToken);

impl Drop for CancelPublicationOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl RegisteredGlobalDb {
    /// The admitted namespace whose handles this client may publish and read:
    /// the exact store-shard identity, never a content digest alone.
    fn response_handle_namespace(&self) -> Result<String, ResponseHandleError> {
        serde_json::to_string(&self.binding().shard_id).map_err(|error| {
            ResponseHandleError::Store(TraceDecayError::Config {
                message: format!("response-handle namespace did not encode: {error}"),
            })
        })
    }

    /// Publishes `content` and returns its handle only after the record has
    /// committed. An identical live record with at least half its lifetime
    /// left answers without a write; an older or expired one is renewed in
    /// place. A different text sharing the truncated digest is refused.
    #[tracing::instrument(name = "global_db.response_handle.publish", level = "trace", skip_all)]
    pub async fn publish_response_handle(
        &self,
        content: &str,
        now: i64,
    ) -> Result<ResponseHandleRecord, ResponseHandleError> {
        let namespace = self.response_handle_namespace()?;
        self.publish_response_handle_in(namespace, content.to_owned(), now, None)
            .await
    }

    /// Publishes for a live request. Writer admission is bounded by its
    /// deadline, and the same fence remains attached through SQL commit even
    /// if the caller drops its wait after the writer accepted the job.
    pub async fn publish_response_handle_with_control(
        &self,
        content: &str,
        deadline: Deadline,
        cancellation: CancellationToken,
        signal: Option<CancellationSignal>,
    ) -> Result<ResponseHandleRecord, ResponseHandleError> {
        let observed_at = now_micros();
        let now = observed_at.0.div_euclid(1_000_000);
        let remaining = deadline.expires_at.0.saturating_sub(observed_at.0).max(0) as u64;
        let expires_at = Instant::now()
            .checked_add(Duration::from_micros(remaining))
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "response_handle_publication_invalid_deadline",
                    false,
                    "Response-handle publication deadline exceeds the monotonic clock",
                )
            })?;
        let control = Arc::new(PublicationControl {
            deadline: MonotonicDeadline::at(expires_at),
            cancellation: cancellation.child_token(),
            signal,
        });
        let _cancel_on_drop = CancelPublicationOnDrop(control.cancellation.clone());
        if let Some(error) = control.refusal() {
            return Err(error.into());
        }
        let namespace = self.response_handle_namespace()?;
        let guard: Arc<dyn ExactSqlWriteAuthority> = control.clone();
        let publication =
            self.publish_response_handle_in(namespace, content.to_owned(), now, Some(guard));
        let deadline_wait = tokio::time::sleep_until(control.deadline.instant().into());
        tokio::pin!(publication);
        tokio::pin!(deadline_wait);
        tokio::select! {
            biased;
            result = &mut publication => result.map_err(|error| match control.refusal() {
                Some(refusal) => ResponseHandleError::Store(refusal),
                None => error,
            }),
            _ = control.cancelled() => Err(TraceDecayError::project_route(
                "response_handle_publication_cancelled",
                true,
                "Response-handle publication was cancelled",
            ).into()),
            _ = &mut deadline_wait => Err(TraceDecayError::project_route(
                "response_handle_publication_timed_out",
                true,
                "Response-handle publication exceeded its request deadline",
            ).into()),
        }
    }

    async fn publish_response_handle_in(
        &self,
        namespace: String,
        content: String,
        now: i64,
        guard: Option<Arc<dyn ExactSqlWriteAuthority>>,
    ) -> Result<ResponseHandleRecord, ResponseHandleError> {
        let digest = content_digest(&content);
        let handle = handle_for_digest(&digest);
        let content_bytes = count(content.len(), "content byte count")?;
        let content_chars = count(content.chars().count(), "content character count")?;
        let expires_at = now.saturating_add(RESPONSE_HANDLE_TTL_SECS);
        self.require_active_write_scope("publish response handle")?;
        self.runtime_database()
            .transact_with_write_guard("publish response handle", guard, async move |transaction| {
                let existing = existing_record(
                    transaction,
                    &namespace,
                    &handle,
                    &content,
                    content_bytes,
                    content_chars,
                )
                .await?;
                let record = |created_at, expires_at, content: &str| ResponseHandleRecord {
                    handle: handle.clone(),
                    created_at,
                    expires_at,
                    content: content.to_owned(),
                };
                match existing {
                    ExistingRecord::Present {
                        digest: stored,
                        created_at,
                        expires_at: stored_expiry,
                        same_content: true,
                    } if stored == digest
                        && created_at <= now
                        && stored_expiry == created_at.saturating_add(RESPONSE_HANDLE_TTL_SECS)
                        && stored_expiry.saturating_sub(now) >= RESPONSE_HANDLE_TTL_SECS / 2 =>
                    {
                        return Ok(record(created_at, stored_expiry, &content));
                    }
                    ExistingRecord::Present { digest: stored, .. }
                        if stored != digest
                            && stored.starts_with(&handle[HANDLE_PREFIX.len()..]) =>
                    {
                        return Err(ResponseHandleError::Collision {
                            handle: handle.clone(),
                        });
                    }
                    // A renewal, an expired record, or a row whose stored
                    // digest or text no longer matches its own handle: the
                    // whole record is rewritten from this content.
                    ExistingRecord::Present { .. } => {
                        transaction
                            .execute(
                                "UPDATE response_handles
                                 SET content_sha256 = ?3, content_bytes = ?4, content_chars = ?5,
                                     created_at = ?6, expires_at = ?7, content = ?8
                                 WHERE namespace = ?1 AND handle = ?2",
                                params![
                                    namespace.as_str(),
                                    handle.as_str(),
                                    digest.as_str(),
                                    content_bytes,
                                    content_chars,
                                    now,
                                    expires_at,
                                    content.as_str(),
                                ],
                            )
                            .await
                            .map_err(store_error("renew response handle"))?;
                    }
                    ExistingRecord::Absent => {
                        transaction
                            .execute(
                                "INSERT INTO response_handles
                                     (namespace, handle, content_sha256, content_bytes,
                                      content_chars, created_at, expires_at, content)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                                params![
                                    namespace.as_str(),
                                    handle.as_str(),
                                    digest.as_str(),
                                    content_bytes,
                                    content_chars,
                                    now,
                                    expires_at,
                                    content.as_str(),
                                ],
                            )
                            .await
                            .map_err(store_error("publish response handle"))?;
                    }
                }
                Ok(record(now, expires_at, &content))
            })
            .await?
    }

    /// Reads one handle of this namespace. The returned content is verified
    /// against the stored digest and the handle; a mismatch is a corrupt
    /// record, never served content. Lookup does not reclaim expired rows.
    #[tracing::instrument(name = "global_db.response_handle.lookup", level = "trace", skip_all)]
    pub async fn lookup_response_handle(
        &self,
        handle: &str,
        now: i64,
    ) -> Result<ResponseHandleLookup, ResponseHandleError> {
        validate_handle(handle)?;
        let namespace = self.response_handle_namespace()?;
        self.lookup_response_handle_in(&namespace, handle, now)
            .await
    }

    async fn lookup_response_handle_in(
        &self,
        namespace: &str,
        handle: &str,
        now: i64,
    ) -> Result<ResponseHandleLookup, ResponseHandleError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(ResponseHandleError::Store)?;
        let mut rows = snapshot
            .query(
                "SELECT content_sha256, content_bytes, content_chars, created_at, expires_at,
                        CASE WHEN expires_at > ?3 THEN content END
                 FROM response_handles WHERE namespace = ?1 AND handle = ?2",
                params![namespace, handle, now],
            )
            .await
            .map_err(store_error("read response handle"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(store_error("decode response handle"))?
        else {
            return Ok(ResponseHandleLookup::Missing);
        };
        let digest = row
            .get::<String>(0)
            .map_err(|error| corrupt(format!("content digest did not decode: {error}")))?;
        let content_bytes = integer(&row, 1, "content byte count")?;
        let content_chars = integer(&row, 2, "content character count")?;
        let created_at = integer(&row, 3, "creation time")?;
        let expires_at = integer(&row, 4, "expiry")?;
        if created_at > now || expires_at != created_at.saturating_add(RESPONSE_HANDLE_TTL_SECS) {
            return Err(corrupt("record has a noncanonical expiry"));
        }
        if expires_at <= now {
            return Ok(ResponseHandleLookup::Expired {
                created_at,
                expires_at,
            });
        }
        let content = row
            .get::<String>(5)
            .map_err(|error| corrupt(format!("content did not decode: {error}")))?;
        if content_digest(&content) != digest || handle_for_digest(&digest) != handle {
            return Err(corrupt("record digest does not match its handle"));
        }
        if count(content.len(), "content byte count")? != content_bytes
            || count(content.chars().count(), "content character count")? != content_chars
        {
            return Err(corrupt("record length does not match its content"));
        }
        Ok(ResponseHandleLookup::Found(ResponseHandleRecord {
            handle: handle.to_owned(),
            created_at,
            expires_at,
            content,
        }))
    }

    /// Removes this namespace's expired rows in bounded batches, each its own
    /// transaction, and returns how many were removed. Rows are selected by
    /// the expiry index alone; a live or foreign-namespace row is never a
    /// candidate, so an interrupted cleanup leaves every valid record.
    #[tracing::instrument(name = "global_db.response_handle.cleanup", level = "trace", skip_all)]
    pub async fn cleanup_expired_response_handles(
        &self,
        now: i64,
    ) -> Result<u64, ResponseHandleError> {
        let namespace = self.response_handle_namespace()?;
        self.cleanup_expired_response_handles_in(namespace, now)
            .await
    }

    async fn cleanup_expired_response_handles_in(
        &self,
        namespace: String,
        now: i64,
    ) -> Result<u64, ResponseHandleError> {
        let mut removed = 0_u64;
        loop {
            let namespace = namespace.clone();
            let batch = self
                .transact(async move |transaction| {
                    transaction
                        .execute(
                            "DELETE FROM response_handles
                             WHERE rowid IN (
                                 SELECT rowid FROM response_handles
                                 WHERE namespace = ?1 AND expires_at <= ?2
                                 LIMIT ?3
                             )",
                            params![namespace.as_str(), now, CLEANUP_BATCH_ROWS],
                        )
                        .await
                        .map_err(store_error("remove expired response handles"))
                })
                .await??;
            removed = removed.saturating_add(batch);
            if batch < CLEANUP_BATCH_ROWS as u64 {
                return Ok(removed);
            }
        }
    }

    /// Counts this namespace's rows from the covering expiry index without
    /// reading any payload.
    #[tracing::instrument(
        name = "global_db.response_handle.inventory",
        level = "trace",
        skip_all
    )]
    pub async fn inventory_response_handles(
        &self,
    ) -> Result<ResponseHandleInventory, ResponseHandleError> {
        let namespace = self.response_handle_namespace()?;
        self.inventory_response_handles_in(&namespace).await
    }

    async fn inventory_response_handles_in(
        &self,
        namespace: &str,
    ) -> Result<ResponseHandleInventory, ResponseHandleError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(ResponseHandleError::Store)?;
        let mut rows = snapshot
            .query(
                "SELECT count(*), coalesce(sum(content_bytes), 0), min(expires_at), max(expires_at)
                 FROM response_handles INDEXED BY idx_response_handles_expiry
                 WHERE namespace = ?1",
                params![namespace],
            )
            .await
            .map_err(store_error("inventory response handles"))?;
        let row = rows
            .next()
            .await
            .map_err(store_error("decode response-handle inventory"))?
            .ok_or_else(|| corrupt("inventory aggregate returned no row"))?;
        let record_count = integer(&row, 0, "record count")?;
        let total_bytes = integer(&row, 1, "byte total")?;
        let bounds = |column, field| {
            row.get::<Option<i64>>(column)
                .map_err(|error| corrupt(format!("{field} did not decode: {error}")))
        };
        Ok(ResponseHandleInventory {
            record_count: u64::try_from(record_count)
                .map_err(|_| corrupt("record count is negative"))?,
            total_bytes: u64::try_from(total_bytes)
                .map_err(|_| corrupt("byte total is negative"))?,
            oldest_expires_at: bounds(2, "oldest expiry")?,
            newest_expires_at: bounds(3, "newest expiry")?,
        })
    }
}

async fn existing_record(
    transaction: &impl QueryExecutor,
    namespace: &str,
    handle: &str,
    content: &str,
    content_bytes: i64,
    content_chars: i64,
) -> Result<ExistingRecord, ResponseHandleError> {
    let mut rows = transaction
        .query(
            "SELECT content_sha256, created_at, expires_at,
                    content = ?3 AND content_bytes = ?4 AND content_chars = ?5
             FROM response_handles WHERE namespace = ?1 AND handle = ?2",
            params![namespace, handle, content, content_bytes, content_chars],
        )
        .await
        .map_err(store_error("read response handle for publication"))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(store_error("decode response handle for publication"))?
    else {
        return Ok(ExistingRecord::Absent);
    };
    Ok(ExistingRecord::Present {
        digest: row
            .get::<String>(0)
            .map_err(|error| corrupt(format!("content digest did not decode: {error}")))?,
        created_at: integer(&row, 1, "creation time")?,
        expires_at: integer(&row, 2, "expiry")?,
        same_content: integer(&row, 3, "content comparison")? == 1,
    })
}

#[cfg(test)]
mod tests;
