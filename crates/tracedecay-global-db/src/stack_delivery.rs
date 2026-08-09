//! Durable GitHub stacked-pull-request signal delivery for one registered DB.
//!
//! This is deliberately separate from the observability outbox.  A signal is
//! an immutable provider/coordinator commitment; recipient rows are the
//! delivery state machine that lets a daemon hand work to a host without
//! mistaking the coordinator's acknowledgement for the host's receipt.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::RegisteredGlobalDb;

/// Maximum number of rows in the `pending` state for one project.
pub const MAX_GITHUB_STACK_ACTIVE_PENDING_V1: usize = 256;
/// A single coordinator/host operation is bounded so a malformed caller
/// cannot turn one transaction into an unbounded fanout.
pub const MAX_GITHUB_STACK_DELIVERY_BATCH_V1: usize = 256;
/// Recipient identity and serialized signal fields are intentionally bounded
/// before they reach the registered store.
const MAX_GITHUB_STACK_ID_BYTES: usize = 512;
const MAX_GITHUB_STACK_SIGNAL_JSON_BYTES: usize = 1_048_576;

/// Neutral, serialized representation of an immutable stack signal.
///
/// The global database does not depend on the use-case crate (which depends on
/// this crate), so the signal payload crosses this boundary as JSON.  The
/// identity columns remain separately queryable and are compared byte-for-byte
/// on replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitHubStackSignalRecordV1 {
    pub project_id: String,
    pub signal_id: String,
    pub scope_digest: String,
    pub repository_id: String,
    pub watermark_id: String,
    pub observed_at_micros: i64,
    pub signal_json: String,
}

/// One immutable signal/recipient binding.  Its state lives in the registered
/// recipient-state table and is read through [`GitHubStackDeliveryStateV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitHubStackDeliveryRecordV1 {
    pub signal: GitHubStackSignalRecordV1,
    pub recipient: String,
}

/// Stable key used by coordinator acknowledgement and host receipt actions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitHubStackDeliveryKeyV1 {
    pub signal_id: String,
    pub recipient: String,
}

impl From<&GitHubStackDeliveryRecordV1> for GitHubStackDeliveryKeyV1 {
    fn from(value: &GitHubStackDeliveryRecordV1) -> Self {
        Self {
            signal_id: value.signal.signal_id.clone(),
            recipient: value.recipient.clone(),
        }
    }
}

/// Durable per-recipient delivery state.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubStackDeliveryStateV1 {
    Pending,
    Deferred,
    HostPending,
    Settled,
    AuthorizationLost,
}

impl GitHubStackDeliveryStateV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Deferred => "deferred",
            Self::HostPending => "host_pending",
            Self::Settled => "settled",
            Self::AuthorizationLost => "authorization_lost",
        }
    }

    fn parse(value: String) -> Result<Self, String> {
        match value.as_str() {
            "pending" => Ok(Self::Pending),
            "deferred" => Ok(Self::Deferred),
            "host_pending" => Ok(Self::HostPending),
            "settled" => Ok(Self::Settled),
            "authorization_lost" => Ok(Self::AuthorizationLost),
            _ => Err(format!("unknown GitHub stack delivery state '{value}'")),
        }
    }
}

/// Outcome of an idempotent signal append.  Saturation is a durable outcome,
/// not a lost write: overflow recipients are retained as `deferred` rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitHubStackSignalAppendOutcomeV1 {
    Appended {
        pending_count: usize,
        deferred_count: usize,
    },
    Replayed {
        pending_count: usize,
        deferred_count: usize,
    },
    Saturated {
        pending_count: usize,
        deferred_count: usize,
    },
}

impl GitHubStackSignalAppendOutcomeV1 {
    pub const fn is_saturated(&self) -> bool {
        matches!(self, Self::Saturated { .. })
    }

    pub const fn pending_count(&self) -> usize {
        match self {
            Self::Appended { pending_count, .. }
            | Self::Replayed { pending_count, .. }
            | Self::Saturated { pending_count, .. } => *pending_count,
        }
    }

    pub const fn deferred_count(&self) -> usize {
        match self {
            Self::Appended { deferred_count, .. }
            | Self::Replayed { deferred_count, .. }
            | Self::Saturated { deferred_count, .. } => *deferred_count,
        }
    }
}

/// Additive schema owned by this module.  It is installed for existing stores
/// as well as fresh stores; no previous registered schema is rewritten.
pub(crate) async fn ensure_github_stack_delivery_schema(
    connection: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS github_stack_delivery_signals (
                project_id TEXT NOT NULL,
                signal_id TEXT NOT NULL,
                scope_digest TEXT NOT NULL,
                repository_id TEXT NOT NULL,
                watermark_id TEXT NOT NULL,
                observed_at_micros INTEGER NOT NULL CHECK(observed_at_micros > 0),
                signal_json TEXT NOT NULL
                    CHECK(json_valid(signal_json) AND length(signal_json) <= 1048576),
                PRIMARY KEY(project_id, signal_id)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS github_stack_delivery_recipients (
                project_id TEXT NOT NULL,
                signal_id TEXT NOT NULL,
                recipient TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN (
                    'pending', 'deferred', 'host_pending', 'settled',
                    'authorization_lost'
                )),
                PRIMARY KEY(project_id, signal_id, recipient),
                FOREIGN KEY(project_id, signal_id)
                    REFERENCES github_stack_delivery_signals(project_id, signal_id)
                    ON DELETE RESTRICT
            ) STRICT;
            CREATE INDEX IF NOT EXISTS idx_github_stack_delivery_pending
                ON github_stack_delivery_recipients(
                    project_id, state, signal_id, recipient
                ) WHERE state = 'pending';
            CREATE INDEX IF NOT EXISTS idx_github_stack_delivery_deferred
                ON github_stack_delivery_recipients(
                    project_id, state, signal_id, recipient
                ) WHERE state = 'deferred';
            CREATE INDEX IF NOT EXISTS idx_github_stack_delivery_host_pending
                ON github_stack_delivery_recipients(
                    project_id, state, signal_id, recipient
                ) WHERE state = 'host_pending';
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_signals_immutable_update
            BEFORE UPDATE ON github_stack_delivery_signals
            BEGIN
                SELECT RAISE(ABORT, 'GitHub stack delivery signals are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_signals_immutable_delete
            BEFORE DELETE ON github_stack_delivery_signals
            BEGIN
                SELECT RAISE(ABORT, 'GitHub stack delivery signals are retained');
            END;
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_recipients_identity_immutable
            BEFORE UPDATE ON github_stack_delivery_recipients
            WHEN OLD.project_id != NEW.project_id
              OR OLD.signal_id != NEW.signal_id
              OR OLD.recipient != NEW.recipient
            BEGIN
                SELECT RAISE(ABORT, 'GitHub stack delivery recipient identity is immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_recipients_transition_guard
            BEFORE UPDATE ON github_stack_delivery_recipients
            WHEN NOT (
                (OLD.state = NEW.state)
                OR (OLD.state = 'pending' AND NEW.state IN ('host_pending', 'authorization_lost'))
                OR (OLD.state = 'deferred' AND NEW.state IN ('pending', 'authorization_lost'))
                OR (OLD.state = 'host_pending' AND NEW.state = 'settled')
            )
            BEGIN
                SELECT RAISE(ABORT, 'invalid GitHub stack delivery state transition');
            END;
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_recipients_no_delete
            BEFORE DELETE ON github_stack_delivery_recipients
            BEGIN
                SELECT RAISE(ABORT, 'GitHub stack delivery recipient rows are retained');
            END;",
        )
        .await
        .map_err(|error| {
            crate::global_db_operation_error("initialize GitHub stack delivery schema", error)
        })
}

fn validate_text(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("GitHub stack {field} must not be empty"));
    }
    if value.len() > MAX_GITHUB_STACK_ID_BYTES {
        return Err(format!(
            "GitHub stack {field} exceeds {MAX_GITHUB_STACK_ID_BYTES} bytes"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("GitHub stack {field} contains a control character"));
    }
    Ok(())
}

fn validate_signal(record: &GitHubStackSignalRecordV1) -> Result<(), String> {
    validate_text(&record.project_id, "project id")?;
    validate_text(&record.signal_id, "signal id")?;
    validate_text(&record.scope_digest, "scope digest")?;
    validate_text(&record.repository_id, "repository id")?;
    validate_text(&record.watermark_id, "watermark id")?;
    if record.observed_at_micros <= 0 {
        return Err("GitHub stack observed timestamp must be positive".to_owned());
    }
    if record.signal_json.len() > MAX_GITHUB_STACK_SIGNAL_JSON_BYTES {
        return Err("GitHub stack signal JSON exceeds 1 MiB".to_owned());
    }
    serde_json::from_str::<serde_json::Value>(&record.signal_json)
        .map_err(|error| format!("GitHub stack signal JSON is invalid: {error}"))?;
    Ok(())
}

fn validate_recipient(recipient: &str) -> Result<(), String> {
    validate_text(recipient, "recipient")
}

fn decode_signal_row(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> Result<GitHubStackSignalRecordV1, String> {
    Ok(GitHubStackSignalRecordV1 {
        project_id: row
            .get::<String>(0)
            .map_err(|error| format!("decode GitHub stack project id: {error}"))?,
        signal_id: row
            .get::<String>(1)
            .map_err(|error| format!("decode GitHub stack signal id: {error}"))?,
        scope_digest: row
            .get::<String>(2)
            .map_err(|error| format!("decode GitHub stack scope digest: {error}"))?,
        repository_id: row
            .get::<String>(3)
            .map_err(|error| format!("decode GitHub stack repository id: {error}"))?,
        watermark_id: row
            .get::<String>(4)
            .map_err(|error| format!("decode GitHub stack watermark id: {error}"))?,
        observed_at_micros: row
            .get::<i64>(5)
            .map_err(|error| format!("decode GitHub stack observed timestamp: {error}"))?,
        signal_json: row
            .get::<String>(6)
            .map_err(|error| format!("decode GitHub stack signal JSON: {error}"))?,
    })
}

async fn state_count(
    executor: &impl QueryExecutor,
    project_id: &str,
    state: GitHubStackDeliveryStateV1,
) -> Result<usize, String> {
    let mut rows = executor
        .query(
            "SELECT COUNT(*) FROM github_stack_delivery_recipients
             WHERE project_id = ?1 AND state = ?2",
            params![project_id, state.as_str()],
        )
        .await
        .map_err(|error| format!("count GitHub stack {state:?} deliveries: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("read GitHub stack {state:?} delivery count: {error}"))?
        .ok_or_else(|| "GitHub stack delivery count row is unavailable".to_owned())?;
    let count = row
        .get::<i64>(0)
        .map_err(|error| format!("decode GitHub stack {state:?} delivery count: {error}"))?;
    usize::try_from(count).map_err(|_| "GitHub stack delivery count is negative".to_owned())
}

async fn counts(executor: &impl QueryExecutor, project_id: &str) -> Result<(usize, usize), String> {
    Ok((
        state_count(executor, project_id, GitHubStackDeliveryStateV1::Pending).await?,
        state_count(executor, project_id, GitHubStackDeliveryStateV1::Deferred).await?,
    ))
}

/// Promotes the oldest deferred rows whenever pending capacity becomes
/// available.  The ordering is explicit so a restart cannot reorder a queue.
async fn promote_deferred(executor: &impl Executor, project_id: &str) -> Result<usize, String> {
    let pending = state_count(executor, project_id, GitHubStackDeliveryStateV1::Pending).await?;
    let capacity = MAX_GITHUB_STACK_ACTIVE_PENDING_V1.saturating_sub(pending);
    if capacity == 0 {
        return Ok(0);
    }
    let limit = i64::try_from(capacity)
        .map_err(|_| "GitHub stack capacity exceeds SQLite range".to_owned())?;
    let mut rows = executor
        .query(
            "SELECT r.signal_id, r.recipient
             FROM github_stack_delivery_recipients AS r
             JOIN github_stack_delivery_signals AS s
               ON s.project_id = r.project_id AND s.signal_id = r.signal_id
             WHERE r.project_id = ?1 AND r.state = 'deferred'
             ORDER BY s.observed_at_micros ASC, r.signal_id ASC, r.recipient ASC
             LIMIT ?2",
            params![project_id, limit],
        )
        .await
        .map_err(|error| format!("query deferred GitHub stack deliveries: {error}"))?;
    let mut promoted = 0;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("read deferred GitHub stack delivery: {error}"))?
    {
        let signal_id = row
            .get::<String>(0)
            .map_err(|error| format!("decode deferred GitHub stack signal id: {error}"))?;
        let recipient = row
            .get::<String>(1)
            .map_err(|error| format!("decode deferred GitHub stack recipient: {error}"))?;
        let changed = executor
            .execute(
                "UPDATE github_stack_delivery_recipients
                 SET state = 'pending'
                 WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                   AND state = 'deferred'",
                params![project_id, signal_id, recipient],
            )
            .await
            .map_err(|error| format!("promote deferred GitHub stack delivery: {error}"))?;
        if changed == 1 {
            promoted += 1;
        }
    }
    Ok(promoted)
}

async fn lookup_signal(
    executor: &impl QueryExecutor,
    project_id: &str,
    signal_id: &str,
) -> Result<Option<GitHubStackSignalRecordV1>, String> {
    let mut rows = executor
        .query(
            "SELECT project_id, signal_id, scope_digest, repository_id,
                    watermark_id, observed_at_micros, signal_json
             FROM github_stack_delivery_signals
             WHERE project_id = ?1 AND signal_id = ?2",
            params![project_id, signal_id],
        )
        .await
        .map_err(|error| format!("query GitHub stack signal: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("read GitHub stack signal: {error}"))?
        .map(|row| decode_signal_row(&row))
        .transpose()
}

impl RegisteredGlobalDb {
    /// Appends one immutable signal and its recipient bindings.  Overflow
    /// bindings are durably deferred and reported as typed saturation.
    pub async fn append_github_stack_signal(
        &self,
        record: GitHubStackSignalRecordV1,
        recipients: Vec<String>,
    ) -> Result<GitHubStackSignalAppendOutcomeV1, String> {
        validate_signal(&record)?;
        let mut recipient_set = BTreeSet::new();
        for recipient in recipients {
            validate_recipient(&recipient)?;
            recipient_set.insert(recipient);
        }
        if recipient_set.len() > MAX_GITHUB_STACK_DELIVERY_BATCH_V1 {
            return Err(format!(
                "GitHub stack signal has more than {} recipients",
                MAX_GITHUB_STACK_DELIVERY_BATCH_V1
            ));
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("begin GitHub stack signal append: {error}"))?;
        if let Some(existing) =
            lookup_signal(&transaction, &record.project_id, &record.signal_id).await?
        {
            if existing != record {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| format!("close conflicting GitHub stack append: {error}"))?;
                return Err("GitHub stack signal identity conflict".to_owned());
            }
            let (pending_count, deferred_count) = counts(&transaction, &record.project_id).await?;
            transaction
                .rollback()
                .await
                .map_err(|error| format!("close replayed GitHub stack append: {error}"))?;
            return Ok(GitHubStackSignalAppendOutcomeV1::Replayed {
                pending_count,
                deferred_count,
            });
        }

        transaction
            .execute(
                "INSERT INTO github_stack_delivery_signals
                 (project_id, signal_id, scope_digest, repository_id,
                  watermark_id, observed_at_micros, signal_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record.project_id.as_str(),
                    record.signal_id.as_str(),
                    record.scope_digest.as_str(),
                    record.repository_id.as_str(),
                    record.watermark_id.as_str(),
                    record.observed_at_micros,
                    record.signal_json.as_str()
                ],
            )
            .await
            .map_err(|error| format!("append GitHub stack signal: {error}"))?;
        // Repair a deferred queue left by an interrupted older writer before
        // assigning this append's capacity.  All callers hold the immediate
        // write transaction, so the quota decision is serialized.
        promote_deferred(&transaction, &record.project_id).await?;
        let (pending_before, _) = counts(&transaction, &record.project_id).await?;
        let capacity = MAX_GITHUB_STACK_ACTIVE_PENDING_V1.saturating_sub(pending_before);
        for (index, recipient) in recipient_set.into_iter().enumerate() {
            let state = if index < capacity {
                GitHubStackDeliveryStateV1::Pending
            } else {
                GitHubStackDeliveryStateV1::Deferred
            };
            transaction
                .execute(
                    "INSERT INTO github_stack_delivery_recipients
                     (project_id, signal_id, recipient, state)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        record.project_id.as_str(),
                        record.signal_id.as_str(),
                        recipient,
                        state.as_str()
                    ],
                )
                .await
                .map_err(|error| format!("append GitHub stack delivery recipient: {error}"))?;
        }
        let (pending_count, deferred_count) = counts(&transaction, &record.project_id).await?;
        let saturated = deferred_count > 0;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit GitHub stack signal append: {error}"))?;
        Ok(if saturated {
            GitHubStackSignalAppendOutcomeV1::Saturated {
                pending_count,
                deferred_count,
            }
        } else {
            GitHubStackSignalAppendOutcomeV1::Appended {
                pending_count,
                deferred_count,
            }
        })
    }

    /// Returns a deterministic page of coordinator-pending deliveries.
    pub async fn pending_github_stack_deliveries(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        validate_text(project_id, "project id")?;
        let limit = limit.min(MAX_GITHUB_STACK_ACTIVE_PENDING_V1);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit)
            .map_err(|_| "GitHub stack page exceeds SQLite range".to_owned())?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("begin GitHub stack pending snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT s.project_id, s.signal_id, s.scope_digest, s.repository_id,
                        s.watermark_id, s.observed_at_micros, s.signal_json, r.recipient
                 FROM github_stack_delivery_recipients AS r
                 JOIN github_stack_delivery_signals AS s
                   ON s.project_id = r.project_id AND s.signal_id = r.signal_id
                 WHERE r.project_id = ?1 AND r.state = 'pending'
                 ORDER BY s.observed_at_micros ASC, r.signal_id ASC, r.recipient ASC
                 LIMIT ?2",
                params![project_id, limit],
            )
            .await
            .map_err(|error| format!("query GitHub stack pending deliveries: {error}"))?;
        let mut deliveries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("read GitHub stack pending delivery: {error}"))?
        {
            let signal = decode_signal_row(&row)?;
            let recipient = row
                .get::<String>(7)
                .map_err(|error| format!("decode GitHub stack pending recipient: {error}"))?;
            deliveries.push(GitHubStackDeliveryRecordV1 { signal, recipient });
        }
        Ok(deliveries)
    }

    /// Publishes a host batch.  This is the durable handoff boundary: rows
    /// become `host_pending`, never `settled`, before a host receipt arrives.
    pub async fn publish_github_stack_deliveries(
        &self,
        project_id: &str,
        watermark_id: &str,
        deliveries: &[GitHubStackDeliveryKeyV1],
    ) -> Result<(), String> {
        self.transition_github_stack_deliveries_to_host_pending(
            project_id,
            watermark_id,
            deliveries,
        )
        .await
    }

    /// Singular convenience form for a host adapter that publishes one
    /// coordinator-selected recipient at a time.
    pub async fn publish_github_stack_delivery(
        &self,
        project_id: &str,
        watermark_id: &str,
        delivery: &GitHubStackDeliveryKeyV1,
    ) -> Result<(), String> {
        self.publish_github_stack_deliveries(
            project_id,
            watermark_id,
            std::slice::from_ref(delivery),
        )
        .await
    }

    /// Coordinator acknowledgement is intentionally not final host
    /// settlement.  It is idempotent for both `pending` and `host_pending`.
    pub async fn acknowledge_github_stack_deliveries(
        &self,
        project_id: &str,
        watermark_id: &str,
        deliveries: &[GitHubStackDeliveryKeyV1],
    ) -> Result<(), String> {
        self.transition_github_stack_deliveries_to_host_pending(
            project_id,
            watermark_id,
            deliveries,
        )
        .await
    }

    /// Singular convenience form for the coordinator's acknowledgement.
    pub async fn acknowledge_github_stack_delivery(
        &self,
        project_id: &str,
        watermark_id: &str,
        delivery: &GitHubStackDeliveryKeyV1,
    ) -> Result<(), String> {
        self.acknowledge_github_stack_deliveries(
            project_id,
            watermark_id,
            std::slice::from_ref(delivery),
        )
        .await
    }

    async fn transition_github_stack_deliveries_to_host_pending(
        &self,
        project_id: &str,
        watermark_id: &str,
        deliveries: &[GitHubStackDeliveryKeyV1],
    ) -> Result<(), String> {
        validate_text(project_id, "project id")?;
        validate_text(watermark_id, "watermark id")?;
        if deliveries.len() > MAX_GITHUB_STACK_DELIVERY_BATCH_V1 {
            return Err(format!(
                "GitHub stack delivery batch exceeds {} rows",
                MAX_GITHUB_STACK_DELIVERY_BATCH_V1
            ));
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("begin GitHub stack host publication: {error}"))?;
        let keys = deliveries.iter().collect::<BTreeSet<_>>();
        for key in keys {
            validate_text(&key.signal_id, "signal id")?;
            validate_recipient(&key.recipient)?;
            let signal = lookup_signal(&transaction, project_id, &key.signal_id)
                .await?
                .ok_or_else(|| "GitHub stack delivery signal is unavailable".to_owned())?;
            if signal.watermark_id != watermark_id {
                return Err("GitHub stack delivery watermark mismatch".to_owned());
            }
            let mut state_rows = transaction
                .query(
                    "SELECT state FROM github_stack_delivery_recipients
                     WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                    params![project_id, key.signal_id.as_str(), key.recipient.as_str()],
                )
                .await
                .map_err(|error| format!("query GitHub stack delivery state: {error}"))?;
            let state = state_rows
                .next()
                .await
                .map_err(|error| format!("read GitHub stack delivery state: {error}"))?
                .ok_or_else(|| "GitHub stack recipient binding is unavailable".to_owned())?
                .get::<String>(0)
                .map_err(|error| format!("decode GitHub stack delivery state: {error}"))?;
            match GitHubStackDeliveryStateV1::parse(state)? {
                GitHubStackDeliveryStateV1::Pending => {
                    transaction
                        .execute(
                            "UPDATE github_stack_delivery_recipients
                             SET state = 'host_pending'
                             WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                               AND state = 'pending'",
                            params![project_id, key.signal_id.as_str(), key.recipient.as_str()],
                        )
                        .await
                        .map_err(|error| format!("publish GitHub stack host delivery: {error}"))?;
                }
                GitHubStackDeliveryStateV1::HostPending => {}
                GitHubStackDeliveryStateV1::Deferred => {
                    return Err("GitHub stack delivery is deferred".to_owned());
                }
                GitHubStackDeliveryStateV1::Settled => {}
                GitHubStackDeliveryStateV1::AuthorizationLost => {
                    return Err("GitHub stack delivery authorization was lost".to_owned());
                }
            }
        }
        promote_deferred(&transaction, project_id).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit GitHub stack host publication: {error}"))
    }

    /// Final host receipt.  Replaying a receipt after settlement is harmless;
    /// settling a row that never reached the host is rejected.
    pub async fn acknowledge_github_stack_host_delivery(
        &self,
        project_id: &str,
        signal_id: &str,
        recipient: &str,
    ) -> Result<(), String> {
        validate_text(project_id, "project id")?;
        validate_text(signal_id, "signal id")?;
        validate_recipient(recipient)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("begin GitHub stack host acknowledgement: {error}"))?;
        let mut rows = transaction
            .query(
                "SELECT state FROM github_stack_delivery_recipients
                 WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                params![project_id, signal_id, recipient],
            )
            .await
            .map_err(|error| format!("query GitHub stack host acknowledgement: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("read GitHub stack host acknowledgement: {error}"))?
        else {
            return Err("GitHub stack recipient binding is unavailable".to_owned());
        };
        let state = GitHubStackDeliveryStateV1::parse(
            row.get::<String>(0)
                .map_err(|error| format!("decode GitHub stack host state: {error}"))?,
        )?;
        match state {
            GitHubStackDeliveryStateV1::HostPending => {
                transaction
                    .execute(
                        "UPDATE github_stack_delivery_recipients
                         SET state = 'settled'
                         WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                           AND state = 'host_pending'",
                        params![project_id, signal_id, recipient],
                    )
                    .await
                    .map_err(|error| format!("settle GitHub stack host delivery: {error}"))?;
            }
            GitHubStackDeliveryStateV1::Settled => {}
            GitHubStackDeliveryStateV1::Pending => {
                return Err("GitHub stack delivery is still pending host publication".to_owned());
            }
            GitHubStackDeliveryStateV1::Deferred => {
                return Err("GitHub stack delivery is deferred".to_owned());
            }
            GitHubStackDeliveryStateV1::AuthorizationLost => {
                return Err("GitHub stack delivery authorization was lost".to_owned());
            }
        }
        promote_deferred(&transaction, project_id).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit GitHub stack host acknowledgement: {error}"))
    }

    /// Marks a pending/deferred recipient as permanently unauthorized.  The
    /// optional outcome is intentionally not persisted: the state itself is
    /// the durable denial authority.
    pub async fn record_github_stack_authorization_loss(
        &self,
        project_id: &str,
        signal_id: &str,
        recipient: &str,
    ) -> Result<(), String> {
        validate_text(project_id, "project id")?;
        validate_text(signal_id, "signal id")?;
        validate_recipient(recipient)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("begin GitHub stack authorization loss: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE github_stack_delivery_recipients
                 SET state = 'authorization_lost'
                 WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                   AND state IN ('pending', 'deferred')",
                params![project_id, signal_id, recipient],
            )
            .await
            .map_err(|error| format!("record GitHub stack authorization loss: {error}"))?;
        if changed == 0 {
            let mut rows = transaction
                .query(
                    "SELECT state FROM github_stack_delivery_recipients
                     WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                    params![project_id, signal_id, recipient],
                )
                .await
                .map_err(|error| format!("query GitHub stack authorization state: {error}"))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| format!("read GitHub stack authorization state: {error}"))?
            else {
                return Err("GitHub stack recipient binding is unavailable".to_owned());
            };
            let state =
                GitHubStackDeliveryStateV1::parse(row.get::<String>(0).map_err(|error| {
                    format!("decode GitHub stack authorization state: {error}")
                })?)?;
            if state != GitHubStackDeliveryStateV1::AuthorizationLost {
                return Err("GitHub stack delivery is no longer authorizable".to_owned());
            }
        }
        promote_deferred(&transaction, project_id).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit GitHub stack authorization loss: {error}"))
    }

    /// Looks up the immutable signal payload by its exact registered-project
    /// identity.
    pub async fn github_stack_signal(
        &self,
        project_id: &str,
        signal_id: &str,
    ) -> Result<Option<GitHubStackSignalRecordV1>, String> {
        validate_text(project_id, "project id")?;
        validate_text(signal_id, "signal id")?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("begin GitHub stack signal lookup: {error}"))?;
        lookup_signal(&snapshot, project_id, signal_id).await
    }

    /// Reads the host-pending handoff page without exposing settled or
    /// authorization-lost bindings.
    pub async fn pending_host_github_stack_deliveries(
        &self,
        project_id: &str,
        scope_digest: &str,
        limit: usize,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        validate_text(project_id, "project id")?;
        validate_text(scope_digest, "scope digest")?;
        let limit = limit.min(MAX_GITHUB_STACK_DELIVERY_BATCH_V1);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit)
            .map_err(|_| "GitHub stack page exceeds SQLite range".to_owned())?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("begin GitHub stack host-pending snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT s.project_id, s.signal_id, s.scope_digest, s.repository_id,
                        s.watermark_id, s.observed_at_micros, s.signal_json, r.recipient
                 FROM github_stack_delivery_recipients AS r
                 JOIN github_stack_delivery_signals AS s
                   ON s.project_id = r.project_id AND s.signal_id = r.signal_id
                 WHERE r.project_id = ?1 AND s.scope_digest = ?2
                   AND r.state = 'host_pending'
                 ORDER BY s.observed_at_micros ASC, r.signal_id ASC, r.recipient ASC
                 LIMIT ?3",
                params![project_id, scope_digest, limit],
            )
            .await
            .map_err(|error| format!("query GitHub stack host-pending deliveries: {error}"))?;
        let mut deliveries = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("read GitHub stack host-pending delivery: {error}"))?
        {
            let signal = decode_signal_row(&row)?;
            let recipient = row
                .get::<String>(7)
                .map_err(|error| format!("decode GitHub stack host-pending recipient: {error}"))?;
            deliveries.push(GitHubStackDeliveryRecordV1 { signal, recipient });
        }
        Ok(deliveries)
    }

    /// Returns one recipient's durable binding state.
    pub async fn github_stack_recipient_state(
        &self,
        project_id: &str,
        signal_id: &str,
        recipient: &str,
    ) -> Result<Option<GitHubStackDeliveryStateV1>, String> {
        validate_text(project_id, "project id")?;
        validate_text(signal_id, "signal id")?;
        validate_recipient(recipient)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("begin GitHub stack recipient state snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT state FROM github_stack_delivery_recipients
                 WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                params![project_id, signal_id, recipient],
            )
            .await
            .map_err(|error| format!("query GitHub stack recipient state: {error}"))?;
        rows.next()
            .await
            .map_err(|error| format!("read GitHub stack recipient state: {error}"))?
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|error| format!("decode GitHub stack recipient state: {error}"))
                    .and_then(GitHubStackDeliveryStateV1::parse)
            })
            .transpose()
    }

    /// Returns the immutable recipient binding when it exists, independent of
    /// whether the binding is pending, settled, or denied.
    pub async fn github_stack_recipient_binding(
        &self,
        project_id: &str,
        signal_id: &str,
        recipient: &str,
    ) -> Result<Option<GitHubStackDeliveryRecordV1>, String> {
        let Some(signal) = self.github_stack_signal(project_id, signal_id).await? else {
            return Ok(None);
        };
        if self
            .github_stack_recipient_state(project_id, signal_id, recipient)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(GitHubStackDeliveryRecordV1 {
            signal,
            recipient: recipient.to_owned(),
        }))
    }
}
