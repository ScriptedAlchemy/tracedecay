//! At-rest privacy rescan over the persisted LCM raw-message store.
//!
//! Ingest sanitizes every raw message before persistence, but rows written
//! under older detector rules can hold values the current detector would
//! redact or refuse. This owner re-evaluates every at-rest raw-message body —
//! inline `content` and whole-message external payload bytes — and re-ingests
//! each hit through the same staging and commit path new ingest uses, so
//! redaction, externalization, quarantine, receipts, and FTS maintenance all
//! follow the one canonical sanitizer. A replaced external payload file is
//! deleted through the payload tombstone machinery so the superseded bytes do
//! not survive on disk.
//!
//! One completed pass settles a per-store watermark keyed by
//! [`lcm_payload_detector_revision`], so the sweep runs once per rule refresh
//! instead of on every project open. An interrupted pass leaves the watermark
//! unset and reruns from the start; sanitization is idempotent. A row the
//! sanitizer cannot re-evaluate fails the run with a typed error — never a
//! silent skip — and the watermark stays unset until a pass covers every row.
//!
//! Media-span payload files are byte ranges the ingest scan already evaluated
//! inside their owning message text before externalizing them; the rescan
//! re-evaluates message bodies (where those placeholders live), not the
//! extracted media bytes. Unreceipted rows are first protected through the
//! existing [`RegisteredGlobalDb::lcm_protect_session_raw_messages`] pass.

use std::path::Path;

use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::privacy::{lcm_payload_detector_revision, sanitize_lcm_payload_text};
use tracedecay_sessions::runtime::{
    SessionMessageRecord,
    lcm::{
        LcmError, LcmStorageKind, gc,
        payload::{self, DeleteOpts},
        raw, schema,
    },
};

use super::RegisteredGlobalDb;

/// Watermark row in `lcm_gc_meta`: the detector revision whose rescan last
/// completed over this store.
pub(crate) const LCM_PRIVACY_RESCAN_META_KEY: &str = "privacy_rescan_completed_revision";

/// One page of raw rows per authority read.
const RESCAN_PAGE_LIMIT: i64 = 64;

/// Truthful outcome of one at-rest LCM privacy rescan request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LcmPrivacyRescanOutcomeV1 {
    /// The store already completed a rescan under the current detector
    /// revision; nothing was scanned.
    AlreadyCurrent,
    /// A full pass ran to completion and settled the watermark.
    Completed(LcmPrivacyRescanReceiptV1),
}

/// Counts of one completed at-rest rescan pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmPrivacyRescanReceiptV1 {
    pub detector_revision: String,
    /// Rows whose at-rest body was re-evaluated under the current detector.
    pub scanned_rows: u64,
    /// Rows the current detector left byte-identical.
    pub clean_rows: u64,
    /// Rows re-ingested because the current detector changed their body or
    /// provider metadata.
    pub remediated_rows: u64,
    /// Unreceipted rows re-protected through the canonical protect pass
    /// before the scan.
    pub protected_rows: u64,
    /// External rows whose payload bytes are no longer at rest (offloaded or
    /// collected); only their placeholder remains, so there is nothing left
    /// to rescan or disclose.
    pub unavailable_payload_rows: u64,
}

/// One at-rest raw row joined with its optional `session_messages` twin.
struct RescanRow {
    store_id: i64,
    provider: String,
    message_id: String,
    session_id: String,
    role: String,
    ordinal: i64,
    timestamp: Option<i64>,
    content: Option<String>,
    storage_kind: LcmStorageKind,
    payload_ref: Option<String>,
    metadata_json: Option<String>,
    projection_kind: Option<String>,
    projection_model: Option<String>,
    projection_tool_names: Option<String>,
    projection_source_path: Option<String>,
    projection_source_offset: Option<i64>,
}

/// The rescan input recovered from one row's at-rest bytes.
enum RescanInput {
    Body {
        text: String,
        replaced_payload_ref: Option<String>,
    },
    /// The external payload bytes are gone (offloaded or collected); the row
    /// serves only its placeholder.
    PayloadUnavailable,
}

impl RegisteredGlobalDb {
    /// Rescans every persisted LCM raw-message body under the current
    /// detector revision, remediating hits through the canonical ingest path.
    /// Runs at most once per store per detector revision.
    pub async fn lcm_privacy_rescan_raw_messages(
        &self,
    ) -> Result<LcmPrivacyRescanOutcomeV1, LcmError> {
        let detector_revision = lcm_payload_detector_revision();
        {
            let snapshot = self.lcm_read_snapshot().await?;
            if schema::get_gc_meta(&snapshot, LCM_PRIVACY_RESCAN_META_KEY)
                .await?
                .as_deref()
                == Some(detector_revision)
            {
                return Ok(LcmPrivacyRescanOutcomeV1::AlreadyCurrent);
            }
        }

        let protected_rows = self.protect_unreceipted_sessions().await?;

        let storage_root = self.lcm_storage_root()?.to_path_buf();
        let mut scanned_rows = 0_u64;
        let mut clean_rows = 0_u64;
        let mut remediated_rows = 0_u64;
        let mut unavailable_payload_rows = 0_u64;
        let mut after_store_id = 0_i64;
        loop {
            let page = self.load_rescan_page(after_store_id).await?;
            let Some(last) = page.last() else {
                break;
            };
            after_store_id = last.store_id;
            for row in page {
                let (text, replaced_payload_ref) =
                    match self.rescan_input(&storage_root, &row).await? {
                        RescanInput::Body {
                            text,
                            replaced_payload_ref,
                        } => (text, replaced_payload_ref),
                        RescanInput::PayloadUnavailable => {
                            unavailable_payload_rows += 1;
                            continue;
                        }
                    };
                scanned_rows += 1;
                let provider_metadata = stored_provider_metadata(&row)?;
                if !requires_remediation(&text, provider_metadata.as_deref())? {
                    clean_rows += 1;
                    continue;
                }
                self.remediate_row(
                    &storage_root,
                    &row,
                    text,
                    provider_metadata,
                    replaced_payload_ref,
                )
                .await?;
                remediated_rows += 1;
            }
        }

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        schema::set_gc_meta(&transaction, LCM_PRIVACY_RESCAN_META_KEY, detector_revision).await?;
        transaction.commit().await?;

        Ok(LcmPrivacyRescanOutcomeV1::Completed(
            LcmPrivacyRescanReceiptV1 {
                detector_revision: detector_revision.to_owned(),
                scanned_rows,
                clean_rows,
                remediated_rows,
                protected_rows,
                unavailable_payload_rows,
            },
        ))
    }

    /// Binds receipts to every unreceipted row through the one existing
    /// protect pass, per owning session.
    async fn protect_unreceipted_sessions(&self) -> Result<u64, LcmError> {
        let sessions = {
            let snapshot = self.lcm_read_snapshot().await?;
            let mut rows = snapshot
                .query(
                    "SELECT DISTINCT provider, session_id
                     FROM lcm_raw_messages
                     WHERE json_extract(
                               metadata_json,
                               '$.ingest_protection.sanitization_receipt'
                           ) IS NULL
                     ORDER BY provider, session_id",
                    (),
                )
                .await?;
            let mut sessions: Vec<(String, String)> = Vec::new();
            while let Some(row) = rows.next().await? {
                sessions.push((row.get(0)?, row.get(1)?));
            }
            sessions
        };
        let mut protected_rows = 0_u64;
        for (provider, session_id) in sessions {
            protected_rows += self
                .lcm_protect_session_raw_messages(&provider, &session_id)
                .await?;
        }
        Ok(protected_rows)
    }

    async fn load_rescan_page(&self, after_store_id: i64) -> Result<Vec<RescanRow>, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT raw.store_id, raw.provider, raw.message_id, raw.session_id,
                        raw.role, raw.ordinal, raw.timestamp, raw.content,
                        raw.storage_kind, raw.payload_ref, raw.metadata_json,
                        message.kind, message.model, message.tool_names,
                        message.source_path, message.source_offset
                 FROM lcm_raw_messages AS raw
                 LEFT JOIN session_messages AS message
                   ON message.provider = raw.provider
                  AND message.message_id = raw.message_id
                 WHERE raw.store_id > ?1
                 ORDER BY raw.store_id
                 LIMIT ?2",
                params![after_store_id, RESCAN_PAGE_LIMIT],
            )
            .await?;
        let mut page = Vec::new();
        while let Some(row) = rows.next().await? {
            let storage_kind_text: String = row.get(8)?;
            let storage_kind = LcmStorageKind::from_db(&storage_kind_text).ok_or_else(|| {
                LcmError::Db(format!("invalid storage_kind: {storage_kind_text}"))
            })?;
            page.push(RescanRow {
                store_id: row.get(0)?,
                provider: row.get(1)?,
                message_id: row.get(2)?,
                session_id: row.get(3)?,
                role: row.get(4)?,
                ordinal: row.get(5)?,
                timestamp: row.get(6)?,
                content: row.get(7)?,
                storage_kind,
                payload_ref: row.get(9)?,
                metadata_json: row.get(10)?,
                projection_kind: row.get(11)?,
                projection_model: row.get(12)?,
                projection_tool_names: row.get(13)?,
                projection_source_path: row.get(14)?,
                projection_source_offset: row.get(15)?,
            });
        }
        Ok(page)
    }

    /// Recovers the at-rest body this row actually serves: inline content, or
    /// the verified external payload bytes.
    async fn rescan_input(
        &self,
        storage_root: &Path,
        row: &RescanRow,
    ) -> Result<RescanInput, LcmError> {
        match row.storage_kind {
            LcmStorageKind::Inline => {
                let text = row
                    .content
                    .clone()
                    .ok_or(LcmError::PayloadIntegrityMismatch)?;
                Ok(RescanInput::Body {
                    text,
                    replaced_payload_ref: None,
                })
            }
            LcmStorageKind::External => {
                let payload_ref = row.payload_ref.as_deref().ok_or_else(|| {
                    LcmError::Db("external raw message carries no payload_ref".to_owned())
                })?;
                let metadata = {
                    let snapshot = self.lcm_read_snapshot().await?;
                    match payload::load_payload_metadata(&snapshot, payload_ref).await {
                        Ok(metadata) => metadata,
                        Err(LcmError::PayloadNotFound | LcmError::PayloadGcd) => {
                            return Ok(RescanInput::PayloadUnavailable);
                        }
                        Err(error) => return Err(error),
                    }
                };
                let byte_count = usize::try_from(metadata.byte_count)
                    .map_err(|_| LcmError::PayloadIntegrityMismatch)?;
                let char_count = usize::try_from(metadata.char_count)
                    .map_err(|_| LcmError::PayloadIntegrityMismatch)?;
                match payload::read_verified_payload_content(
                    storage_root,
                    payload_ref,
                    &metadata.content_hash,
                    byte_count,
                    char_count,
                ) {
                    Ok(text) => Ok(RescanInput::Body {
                        text,
                        replaced_payload_ref: Some(payload_ref.to_owned()),
                    }),
                    Err(LcmError::PayloadMissing | LcmError::PayloadGcd) => {
                        Ok(RescanInput::PayloadUnavailable)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// Re-ingests one dirty row through the canonical staging and commit
    /// path, resynchronizes its projection twin, and tombstones a replaced
    /// external payload so the superseded bytes leave the disk.
    async fn remediate_row(
        &self,
        storage_root: &Path,
        row: &RescanRow,
        text: String,
        provider_metadata_json: Option<String>,
        replaced_payload_ref: Option<String>,
    ) -> Result<(), LcmError> {
        let record = SessionMessageRecord {
            provider: row.provider.clone(),
            message_id: row.message_id.clone(),
            session_id: row.session_id.clone(),
            role: row.role.clone(),
            timestamp: row.timestamp,
            ordinal: row.ordinal,
            text,
            kind: row.projection_kind.clone(),
            model: row.projection_model.clone(),
            tool_names: row.projection_tool_names.clone(),
            source_path: row.projection_source_path.clone(),
            source_offset: row.projection_source_offset,
            metadata_json: provider_metadata_json,
        };
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let staged = raw::stage_raw_message_with_payload_tracked(
            storage_root,
            &record,
            &mut payload_rollback,
        )?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let upsert = raw::commit_staged_raw_message(&transaction, &record, staged).await?;
        transaction
            .execute(
                "UPDATE session_messages SET text = ?3, metadata_json = ?4
                 WHERE provider = ?1 AND message_id = ?2",
                params![
                    record.provider.as_str(),
                    record.message_id.as_str(),
                    upsert.projection_text.as_str(),
                    upsert.projection_metadata_json.as_deref(),
                ],
            )
            .await?;
        // An external row remediates only when its body changed, and payload
        // refs are content-addressed, so the re-ingest can never reuse the
        // replaced ref: the superseded payload is always safe to delete.
        if let Some(old_ref) = replaced_payload_ref.as_deref() {
            payload::delete_external_payload_in_transaction(
                &transaction,
                storage_root,
                old_ref,
                &DeleteOpts {
                    rewrite_placeholders: false,
                    remove_file: true,
                    verify_hash: false,
                },
            )
            .await?;
        }
        transaction.commit().await?;
        payload_rollback.disarm();
        if let Some(old_ref) = replaced_payload_ref.as_deref() {
            let transaction = self
                .begin_write_transaction()
                .await
                .map_err(|error| LcmError::Db(error.to_string()))?;
            gc::drain_pending_payload_delete_in_transaction(&transaction, storage_root, old_ref)
                .await?;
            transaction.commit().await?;
        }
        Ok(())
    }
}

/// Returns whether the current detector would change this row's served body
/// or provider metadata. A payload the detector refuses to re-evaluate fails
/// the rescan with a typed error instead of passing as clean.
fn requires_remediation(text: &str, provider_metadata: Option<&str>) -> Result<bool, LcmError> {
    let sanitization =
        sanitize_lcm_payload_text(text).map_err(|error| LcmError::SanitizationRefused {
            reason: format!("at-rest LCM privacy rescan refused a stored payload: {error}"),
        })?;
    if sanitization.sanitized_text() != text {
        return Ok(true);
    }
    match provider_metadata {
        Some(metadata) => raw::provider_metadata_requires_resanitization(metadata),
        None => Ok(false),
    }
}

/// Recovers the provider metadata a re-ingest must carry: the stored metadata
/// without the `ingest_protection` envelope the ingest path re-derives.
///
/// Whole-message external rows never persisted provider metadata (their
/// stored metadata is the payload envelope ingest builds fresh), so they
/// re-ingest with none — exactly what ingest produced the first time.
fn stored_provider_metadata(row: &RescanRow) -> Result<Option<String>, LcmError> {
    if row.storage_kind == LcmStorageKind::External {
        return Ok(None);
    }
    let Some(metadata_json) = row.metadata_json.as_deref() else {
        return Ok(None);
    };
    let mut metadata =
        serde_json::from_str::<serde_json::Value>(metadata_json).map_err(|error| {
            LcmError::SanitizationRefused {
                reason: format!("stored LCM metadata is not valid JSON: {error}"),
            }
        })?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| LcmError::SanitizationRefused {
            reason: "stored LCM metadata must be a JSON object".to_owned(),
        })?;
    object.remove("ingest_protection");
    if object.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&metadata)
        .map(Some)
        .map_err(|error| LcmError::Db(format!("LCM metadata encoding failed: {error}")))
}
