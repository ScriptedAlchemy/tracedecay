//! Durable GitHub stacked-pull-request signal delivery for one registered DB.
//!
//! This is deliberately separate from the observability outbox.  A signal is
//! an immutable provider/coordinator commitment; recipient rows are the
//! delivery state machine.  A row stays `pending` until the authenticated host
//! read that expands it records the receipt, so listing never settles.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params, params_from_iter};

use crate::{RegisteredGlobalDb, registered::JobExit};

/// Maximum number of rows in the `pending` state for one recipient of one
/// project.  A recipient that never reads cannot exhaust another's capacity.
pub const MAX_GITHUB_STACK_ACTIVE_PENDING_V1: usize = 256;
/// A single read page or signal fanout is bounded so a malformed caller
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

/// Durable per-recipient delivery state.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubStackDeliveryStateV1 {
    Pending,
    Deferred,
    Settled,
    AuthorizationLost,
}

impl GitHubStackDeliveryStateV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Deferred => "deferred",
            Self::Settled => "settled",
            Self::AuthorizationLost => "authorization_lost",
        }
    }

    fn parse(value: String) -> Result<Self, String> {
        match value.as_str() {
            "pending" => Ok(Self::Pending),
            "deferred" => Ok(Self::Deferred),
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

/// Schema owned by this module, installed for existing stores as well as fresh
/// ones. Deferred overflow is promoted under the per-recipient bound. Every
/// statement is a no-op once applied.
pub(crate) async fn ensure_github_stack_delivery_schema(
    connection: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    let schema = format!(
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
                    'pending', 'deferred', 'settled', 'authorization_lost'
                )),
                PRIMARY KEY(project_id, signal_id, recipient),
                FOREIGN KEY(project_id, signal_id)
                    REFERENCES github_stack_delivery_signals(project_id, signal_id)
                    ON DELETE RESTRICT
            ) STRICT;
            CREATE INDEX IF NOT EXISTS idx_github_stack_delivery_pending_recipient
                ON github_stack_delivery_recipients(
                    project_id, recipient, signal_id
                ) WHERE state = 'pending';
            CREATE INDEX IF NOT EXISTS idx_github_stack_delivery_deferred
                ON github_stack_delivery_recipients(
                    project_id, recipient, signal_id
                ) WHERE state = 'deferred';
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
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_recipients_settlement_guard
            BEFORE UPDATE ON github_stack_delivery_recipients
            WHEN NOT (
                (OLD.state = NEW.state)
                OR (OLD.state = 'pending' AND NEW.state IN ('settled', 'authorization_lost'))
                OR (OLD.state = 'deferred' AND NEW.state IN ('pending', 'authorization_lost'))
            )
            BEGIN
                SELECT RAISE(ABORT, 'invalid GitHub stack delivery state transition');
            END;
            CREATE TRIGGER IF NOT EXISTS github_stack_delivery_recipients_no_delete
            BEFORE DELETE ON github_stack_delivery_recipients
            BEGIN
                SELECT RAISE(ABORT, 'GitHub stack delivery recipient rows are retained');
            END;
            {promote}",
        promote = promote_deferred_sql("")
    );
    connection.execute_batch(&schema).await.map_err(|error| {
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

// The state is a literal so the per-state partial indexes stay usable; the
// planner does not match a bound value against an index predicate.
fn state_count_sql(state: GitHubStackDeliveryStateV1) -> String {
    format!(
        "SELECT COUNT(*) FROM github_stack_delivery_recipients
         WHERE project_id = ?1 AND state = '{}'",
        state.as_str()
    )
}

async fn state_count(
    executor: &impl QueryExecutor,
    project_id: &str,
    state: GitHubStackDeliveryStateV1,
) -> Result<usize, String> {
    let mut rows = executor
        .query(&state_count_sql(state), params![project_id])
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

/// Promotes each recipient's oldest deferred rows into whatever per-recipient
/// pending capacity is free.  `scope_filter` narrows the candidates (`""`
/// covers every project, as the schema migration needs).  The ordering is
/// explicit so a restart cannot reorder a queue.
fn promote_deferred_sql(scope_filter: &str) -> String {
    format!(
        "UPDATE github_stack_delivery_recipients
         SET state = 'pending'
         WHERE state = 'deferred' AND rowid IN (
             SELECT ranked.rowid FROM (
                 SELECT d.rowid AS rowid,
                        ROW_NUMBER() OVER (
                            PARTITION BY d.project_id, d.recipient
                            ORDER BY s.observed_at_micros ASC, d.signal_id ASC
                        ) AS queue_rank,
                        (SELECT COUNT(*) FROM github_stack_delivery_recipients AS p
                         WHERE p.project_id = d.project_id
                           AND p.recipient = d.recipient
                           AND p.state = 'pending') AS pending_count
                 FROM github_stack_delivery_recipients AS d
                 JOIN github_stack_delivery_signals AS s
                   ON s.project_id = d.project_id AND s.signal_id = d.signal_id
                 WHERE d.state = 'deferred'{scope_filter}
             ) AS ranked
             WHERE ranked.queue_rank <= {MAX_GITHUB_STACK_ACTIVE_PENDING_V1} - ranked.pending_count
         );"
    )
}

#[tracing::instrument(
    name = "global_db.stack_delivery.queue.promote",
    level = "trace",
    skip_all
)]
async fn promote_deferred(
    executor: &impl Executor,
    project_id: &str,
    recipient: &str,
) -> Result<(), String> {
    executor
        .execute(
            &promote_deferred_sql(" AND d.project_id = ?1 AND d.recipient = ?2"),
            params![project_id, recipient],
        )
        .await
        .map_err(|error| format!("promote deferred GitHub stack deliveries: {error}"))?;
    Ok(())
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

/// Recipients among `recipients` whose pending rows already fill their bound.
async fn saturated_recipients(
    executor: &impl QueryExecutor,
    project_id: &str,
    recipients: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    if recipients.is_empty() {
        return Ok(BTreeSet::new());
    }
    let placeholders = (0..recipients.len())
        .map(|index| format!("?{}", index + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let mut parameters = Vec::with_capacity(recipients.len() + 1);
    parameters.push(project_id);
    parameters.extend(recipients.iter().map(String::as_str));
    let mut rows = executor
        .query(
            &format!(
                "SELECT recipient FROM github_stack_delivery_recipients
                 WHERE project_id = ?1 AND state = 'pending'
                   AND recipient IN ({placeholders})
                 GROUP BY recipient
                 HAVING COUNT(*) >= {MAX_GITHUB_STACK_ACTIVE_PENDING_V1}"
            ),
            params_from_iter(parameters),
        )
        .await
        .map_err(|error| format!("query saturated GitHub stack recipients: {error}"))?;
    let mut saturated = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("read saturated GitHub stack recipient: {error}"))?
    {
        saturated.insert(
            row.get::<String>(0)
                .map_err(|error| format!("decode saturated GitHub stack recipient: {error}"))?,
        );
    }
    Ok(saturated)
}

impl RegisteredGlobalDb {
    /// Appends one immutable signal and its recipient bindings.  A recipient
    /// whose pending bound is full has its binding durably deferred, reported
    /// as typed saturation.
    #[tracing::instrument(
        name = "global_db.stack_delivery.persist.append",
        level = "trace",
        skip_all
    )]
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
        self.transact_or_rollback(
            |error| format!("GitHub stack signal append: {error}"),
            async move |transaction| {
                if let Some(existing) =
                    lookup_signal(&transaction, &record.project_id, &record.signal_id).await?
                {
                    if existing != record {
                        return Err("GitHub stack signal identity conflict".to_owned().into());
                    }
                    let (pending_count, deferred_count) =
                        counts(&transaction, &record.project_id).await?;
                    return Err(JobExit::Rollback(
                        GitHubStackSignalAppendOutcomeV1::Replayed {
                            pending_count,
                            deferred_count,
                        },
                    ));
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
                // The write job is serialized, so the quota decision is too.
                let saturated =
                    saturated_recipients(&transaction, &record.project_id, &recipient_set).await?;
                for recipient in &recipient_set {
                    let state = if saturated.contains(recipient) {
                        GitHubStackDeliveryStateV1::Deferred
                    } else {
                        GitHubStackDeliveryStateV1::Pending
                    };
                    transaction
                        .execute(
                            "INSERT INTO github_stack_delivery_recipients
                         (project_id, signal_id, recipient, state)
                         VALUES (?1, ?2, ?3, ?4)",
                            params![
                                record.project_id.as_str(),
                                record.signal_id.as_str(),
                                recipient.as_str(),
                                state.as_str()
                            ],
                        )
                        .await
                        .map_err(|error| {
                            format!("append GitHub stack delivery recipient: {error}")
                        })?;
                }
                let (pending_count, deferred_count) =
                    counts(&transaction, &record.project_id).await?;
                Ok(if deferred_count > 0 {
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
            },
        )
        .await
    }

    /// Returns a deterministic page of one scope's `pending` deliveries,
    /// oldest first, for one recipient or for every recipient.  Reading does
    /// not change delivery state; only [`Self::acknowledge_github_stack_host_delivery`]
    /// settles.  The page is the per-recipient pending bound, so one
    /// recipient's whole backlog fits in it.
    #[tracing::instrument(
        name = "global_db.stack_delivery.query.pending_page",
        level = "trace",
        skip_all
    )]
    pub async fn pending_github_stack_deliveries(
        &self,
        project_id: &str,
        scope_digest: &str,
        recipient: Option<&str>,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        validate_text(project_id, "project id")?;
        validate_text(scope_digest, "scope digest")?;
        if let Some(recipient) = recipient {
            validate_recipient(recipient)?;
        }
        let limit = i64::try_from(MAX_GITHUB_STACK_ACTIVE_PENDING_V1)
            .map_err(|_| "GitHub stack page exceeds SQLite range".to_owned())?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("begin GitHub stack pending snapshot: {error}"))?;
        let select = "SELECT s.project_id, s.signal_id, s.scope_digest, s.repository_id,
                            s.watermark_id, s.observed_at_micros, s.signal_json, r.recipient
                     FROM github_stack_delivery_recipients AS r
                     JOIN github_stack_delivery_signals AS s
                       ON s.project_id = r.project_id AND s.signal_id = r.signal_id
                     WHERE r.project_id = ?1 AND s.scope_digest = ?2
                       AND r.state = 'pending'";
        let order = format!(
            "ORDER BY s.observed_at_micros ASC, r.signal_id ASC, r.recipient ASC LIMIT {limit}"
        );
        let mut rows = match recipient {
            Some(recipient) => {
                snapshot
                    .query(
                        &format!("{select} AND r.recipient = ?3 {order}"),
                        params![project_id, scope_digest, recipient],
                    )
                    .await
            }
            None => {
                snapshot
                    .query(
                        &format!("{select} {order}"),
                        params![project_id, scope_digest],
                    )
                    .await
            }
        }
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

    /// Host receipt, recorded by the authenticated read that expanded the
    /// signal.  Replaying it after settlement is harmless; settling a row that
    /// is deferred or lost its authorization is rejected.
    #[tracing::instrument(
        name = "global_db.stack_delivery.persist.settle",
        level = "trace",
        skip_all
    )]
    pub async fn acknowledge_github_stack_host_delivery(
        &self,
        project_id: &str,
        signal_id: &str,
        recipient: &str,
    ) -> Result<(), String> {
        validate_text(project_id, "project id")?;
        validate_text(signal_id, "signal id")?;
        validate_recipient(recipient)?;
        let (project_id, signal_id, recipient) = (
            project_id.to_owned(),
            signal_id.to_owned(),
            recipient.to_owned(),
        );
        self.transact_or(
            |error| format!("GitHub stack host acknowledgement: {error}"),
            async move |transaction| {
                let mut rows = transaction
                    .query(
                        "SELECT state FROM github_stack_delivery_recipients
                         WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                        params![project_id.as_str(), signal_id.as_str(), recipient.as_str()],
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
                    GitHubStackDeliveryStateV1::Pending => {
                        transaction
                            .execute(
                                "UPDATE github_stack_delivery_recipients
                                 SET state = 'settled'
                                 WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                                   AND state = 'pending'",
                                params![
                                    project_id.as_str(),
                                    signal_id.as_str(),
                                    recipient.as_str()
                                ],
                            )
                            .await
                            .map_err(|error| {
                                format!("settle GitHub stack host delivery: {error}")
                            })?;
                    }
                    GitHubStackDeliveryStateV1::Settled => {}
                    GitHubStackDeliveryStateV1::Deferred => {
                        return Err("GitHub stack delivery is deferred".to_owned());
                    }
                    GitHubStackDeliveryStateV1::AuthorizationLost => {
                        return Err("GitHub stack delivery authorization was lost".to_owned());
                    }
                }
                promote_deferred(&transaction, &project_id, &recipient).await?;
                Ok(())
            },
        )
        .await
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
        let (project_id, signal_id, recipient) = (
            project_id.to_owned(),
            signal_id.to_owned(),
            recipient.to_owned(),
        );
        self.transact_or(
            |error| format!("GitHub stack authorization loss: {error}"),
            async move |transaction| {
                let changed = transaction
                    .execute(
                        "UPDATE github_stack_delivery_recipients
                         SET state = 'authorization_lost'
                         WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3
                           AND state IN ('pending', 'deferred')",
                        params![project_id.as_str(), signal_id.as_str(), recipient.as_str()],
                    )
                    .await
                    .map_err(|error| format!("record GitHub stack authorization loss: {error}"))?;
                if changed == 0 {
                    let mut rows = transaction
                        .query(
                            "SELECT state FROM github_stack_delivery_recipients
                             WHERE project_id = ?1 AND signal_id = ?2 AND recipient = ?3",
                            params![project_id.as_str(), signal_id.as_str(), recipient.as_str()],
                        )
                        .await
                        .map_err(|error| {
                            format!("query GitHub stack authorization state: {error}")
                        })?;
                    let Some(row) = rows.next().await.map_err(|error| {
                        format!("read GitHub stack authorization state: {error}")
                    })?
                    else {
                        return Err("GitHub stack recipient binding is unavailable".to_owned());
                    };
                    let state = GitHubStackDeliveryStateV1::parse(row.get::<String>(0).map_err(
                        |error| format!("decode GitHub stack authorization state: {error}"),
                    )?)?;
                    if state != GitHubStackDeliveryStateV1::AuthorizationLost {
                        return Err("GitHub stack delivery is no longer authorizable".to_owned());
                    }
                }
                promote_deferred(&transaction, &project_id, &recipient).await?;
                Ok(())
            },
        )
        .await
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
}

#[cfg(test)]
mod plan_tests {
    use super::{GitHubStackDeliveryStateV1, state_count_sql};
    use crate::tests::harness::RegisteredGlobalDbHarness;

    #[asupersync::test]
    async fn state_counts_search_their_partial_indexes() {
        let harness = RegisteredGlobalDbHarness::open("github-stack-delivery-count-plan").await;
        let snapshot = harness.registered.read_snapshot().await.unwrap();
        for (state, index) in [
            (
                GitHubStackDeliveryStateV1::Pending,
                "idx_github_stack_delivery_pending_recipient",
            ),
            (
                GitHubStackDeliveryStateV1::Deferred,
                "idx_github_stack_delivery_deferred",
            ),
        ] {
            let sql = state_count_sql(state);
            let mut rows = snapshot
                .query(
                    &format!("EXPLAIN QUERY PLAN {sql}"),
                    ["project.github-stack-count-plan"],
                )
                .await
                .unwrap();
            let mut plan = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                plan.push(row.get::<String>(3).unwrap());
            }
            assert!(
                plan.iter().any(|detail| detail.contains(index)),
                "{sql}\nmust search {index}: {plan:?}"
            );
        }
    }
}
