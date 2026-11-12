//! Bounded encoded regions and publication references in the project database.

use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_domain::ManifestDigest;
use tracedecay_store::runtime::{
    CODE_INDEX_REGION_BYTES, CodeIndexAllocationAdmission, CodeIndexGenerationKey,
    CodeIndexGenerationPage, CodeIndexGenerationRecord,
    CodeIndexPayloadIdentity, CodeIndexPublicationStage, CodeIndexPublishOutcome,
    CodeIndexStorageError, CodeIndexStorageResult, StoreOperationIdV1,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlRow, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction,
    ExactSqlValue,
};
use crate::repository::RetainedExactSqlCapability;

pub use tracedecay_store::runtime::CodeIndexStoredBytes;

pub const CODE_INDEX_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS code_index_payloads (
    digest TEXT PRIMARY KEY NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    stored_bytes INTEGER NOT NULL DEFAULT 0 CHECK (stored_bytes >= 0 AND stored_bytes <= size_bytes),
    region_count INTEGER NOT NULL DEFAULT 0 CHECK (region_count >= 0),
    sealed INTEGER NOT NULL CHECK (sealed IN (0, 1, 2))
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_regions (
    digest TEXT NOT NULL REFERENCES code_index_payloads(digest) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    body BLOB NOT NULL CHECK (length(body) > 0 AND length(body) <= 1048576),
    PRIMARY KEY (digest, ordinal)
) STRICT;
CREATE TRIGGER IF NOT EXISTS code_index_regions_no_update
BEFORE UPDATE ON code_index_regions BEGIN
    SELECT RAISE(ABORT, 'encoded code-index regions are immutable');
END;
CREATE TRIGGER IF NOT EXISTS code_index_regions_contiguous_insert
BEFORE INSERT ON code_index_regions
WHEN NOT EXISTS (
    SELECT 1 FROM code_index_payloads p
    WHERE p.digest = NEW.digest AND p.sealed = 0 AND p.region_count = NEW.ordinal
        AND length(NEW.body) = MIN(1048576, p.size_bytes - p.stored_bytes)
) BEGIN
    SELECT RAISE(ABORT, 'encoded code-index region is not the next exact extent');
END;
CREATE TRIGGER IF NOT EXISTS code_index_regions_account_insert
AFTER INSERT ON code_index_regions BEGIN
    UPDATE code_index_payloads SET stored_bytes = stored_bytes + length(NEW.body),
        region_count = region_count + 1 WHERE digest = NEW.digest;
END;
CREATE TRIGGER IF NOT EXISTS code_index_regions_no_delete
BEFORE DELETE ON code_index_regions
WHEN EXISTS (SELECT 1 FROM code_index_payloads p WHERE p.digest = OLD.digest AND p.sealed != 2) BEGIN
    SELECT RAISE(ABORT, 'encoded regions may only be reclaimed with their payload');
END;
CREATE TABLE IF NOT EXISTS code_index_staging (
    operation_id TEXT PRIMARY KEY NOT NULL,
    authority_owner TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    expected_head BLOB CHECK (expected_head IS NULL OR length(expected_head) <= 524288),
    UNIQUE (worktree_id, generation_id)
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_staging_refs (
    operation_id TEXT NOT NULL REFERENCES code_index_staging(operation_id) ON DELETE CASCADE,
    digest TEXT NOT NULL REFERENCES code_index_payloads(digest),
    PRIMARY KEY (operation_id, digest)
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_staging_pages (
    operation_id TEXT NOT NULL REFERENCES code_index_staging(operation_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    digest TEXT NOT NULL,
    body BLOB NOT NULL CHECK (length(body) > 0 AND length(body) <= 1048576),
    PRIMARY KEY (operation_id, ordinal)
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_generations (
    worktree_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    manifest_digest TEXT NOT NULL REFERENCES code_index_payloads(digest),
    manifest_size INTEGER NOT NULL CHECK (manifest_size >= 0),
    publication_head BLOB NOT NULL CHECK (length(publication_head) > 0 AND length(publication_head) <= 524288),
    PRIMARY KEY (worktree_id, generation_id)
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_generation_refs (
    worktree_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    digest TEXT NOT NULL REFERENCES code_index_payloads(digest),
    PRIMARY KEY (worktree_id, generation_id, digest),
    FOREIGN KEY (worktree_id, generation_id)
        REFERENCES code_index_generations(worktree_id, generation_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX IF NOT EXISTS code_index_generation_refs_digest
    ON code_index_generation_refs(digest);
CREATE TABLE IF NOT EXISTS code_index_heads (
    worktree_id TEXT PRIMARY KEY NOT NULL,
    generation_id TEXT NOT NULL,
    head BLOB NOT NULL CHECK (length(head) > 0 AND length(head) <= 524288),
    FOREIGN KEY (worktree_id, generation_id)
        REFERENCES code_index_generations(worktree_id, generation_id)
) STRICT;
CREATE TABLE IF NOT EXISTS code_index_retirement_receipts (
    operation_id TEXT PRIMARY KEY NOT NULL,
    worktree_id TEXT NOT NULL,
    generation_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'complete')),
    receipt BLOB NOT NULL CHECK (length(receipt) > 0 AND length(receipt) <= 1048576),
    UNIQUE (worktree_id, generation_id)
) STRICT;
CREATE TRIGGER IF NOT EXISTS code_index_heads_no_retiring_insert
BEFORE INSERT ON code_index_heads
WHEN EXISTS (
    SELECT 1 FROM code_index_retirement_receipts r
    WHERE r.worktree_id = NEW.worktree_id AND (
        r.generation_id = NEW.generation_id OR EXISTS (
            SELECT 1 FROM json_each(CASE WHEN json_valid(CAST(NEW.head AS TEXT))
                THEN CAST(NEW.head AS TEXT) ELSE '{}' END, '$.generation_index') e
            WHERE json_extract(e.value, '$.generation_id') = r.generation_id
        )
    )
) BEGIN
    SELECT RAISE(ABORT, 'retiring code-index generation cannot be adopted');
END;
CREATE TRIGGER IF NOT EXISTS code_index_heads_no_retiring_update
BEFORE UPDATE ON code_index_heads
WHEN EXISTS (
    SELECT 1 FROM code_index_retirement_receipts r
    WHERE r.worktree_id = NEW.worktree_id AND (
        r.generation_id = NEW.generation_id OR EXISTS (
            SELECT 1 FROM json_each(CASE WHEN json_valid(CAST(NEW.head AS TEXT))
                THEN CAST(NEW.head AS TEXT) ELSE '{}' END, '$.generation_index') e
            WHERE json_extract(e.value, '$.generation_id') = r.generation_id
        )
    )
) BEGIN
    SELECT RAISE(ABORT, 'retiring code-index generation cannot be adopted');
END;
CREATE TRIGGER IF NOT EXISTS code_index_replay_no_retiring_insert
BEFORE INSERT ON graph_publication_replay_v1
WHEN EXISTS (
    SELECT 1 FROM code_index_retirement_receipts r
    WHERE r.generation_id = json_extract(CAST(NEW.canonical_replay_source AS TEXT), '$.sealed_code_generation.generation')
        AND r.manifest_digest = json_extract(CAST(NEW.canonical_replay_source AS TEXT), '$.sealed_code_generation.sealed_state_digest')
) BEGIN
    SELECT RAISE(ABORT, 'retiring code-index generation cannot acquire a graph replay');
END;
";

const MAX_PUBLICATION_HEAD_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub struct CodeIndexSqliteStorage {
    retained: RetainedExactSqlCapability,
    admission: Arc<dyn CodeIndexAllocationAdmission>,
    authority_owner: Arc<str>,
}

impl std::fmt::Debug for CodeIndexSqliteStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CodeIndexSqliteStorage")
            .field("binding", self.retained.handle().binding())
            .finish_non_exhaustive()
    }
}

impl CodeIndexSqliteStorage {
    pub fn binding(&self) -> &tracedecay_store::StoreRuntimeBindingV1 {
        self.retained.handle().binding()
    }

    pub fn verified_locator(&self) -> &tracedecay_store::VerifiedStoreLocatorV1 {
        self.retained.handle().verified_locator()
    }

    pub fn from_retained_exact_sql(
        retained: RetainedExactSqlCapability,
        admission: Arc<dyn CodeIndexAllocationAdmission>,
        authority_owner: String,
    ) -> Self {
        Self { retained, admission, authority_owner: Arc::from(authority_owner) }
    }

    /// Charge an owned publication/receipt result before its producer writes it.
    /// The guard travels with the output through its final consumer.
    pub fn encode_owned(
        &self,
        size: usize,
        fill: impl FnOnce(&mut [u8]) -> CodeIndexStorageResult<()>,
    ) -> CodeIndexStorageResult<CodeIndexStoredBytes> {
        let reservation = self.admission.reserve(allocation_cost(0, size)?)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size)
            .map_err(|error| CodeIndexStorageError::AllocationRefused(error.to_string()))?;
        bytes.resize(size, 0);
        fill(&mut bytes)?;
        Ok(CodeIndexStoredBytes::from_reserved(bytes, reservation))
    }

    pub fn encode_json(
        &self,
        value: &impl serde::Serialize,
    ) -> CodeIndexStorageResult<CodeIndexStoredBytes> {
        struct Extent(usize);
        impl std::io::Write for Extent {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.checked_add(bytes.len())
                    .ok_or_else(|| std::io::Error::other("serialized extent overflow"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let mut extent = Extent(0);
        serde_json::to_writer(&mut extent, value).map_err(|error| corrupt(error.to_string()))?;
        self.encode_owned(extent.0, |mut output| {
            serde_json::to_writer(&mut output, value).map_err(|error| corrupt(error.to_string()))?;
            if !output.is_empty() {
                return Err(corrupt("serialized output differs from its measured extent"));
            }
            Ok(())
        })
    }

    pub fn manifest_generation(
        &self,
        generation: &tracedecay_domain::CodeGenerationId,
        digest: &ManifestDigest,
    ) -> CodeIndexStorageResult<Option<CodeIndexGenerationKey>> {
        let rows = self.retained.handle().query(statement(
            "SELECT worktree_id FROM code_index_generations
             WHERE generation_id = ?1 AND manifest_digest = ?2 ORDER BY worktree_id LIMIT 1",
            vec![text(generation.as_str()), text(digest.as_str())],
        )?, Duration::from_secs(5)).map_err(sql_error)?;
        one_row(rows)?.map(|row| {
            let mut values = row.values.into_iter();
            match (values.next(), values.next()) {
                (Some(ExactSqlValue::Text(worktree)), None) => {
                    let worktree_id = tracedecay_domain::WorktreeId::new(worktree)
                        .map_err(|error| corrupt(error.to_string()))?;
                    Ok(CodeIndexGenerationKey { worktree_id, generation_id: generation.clone() })
                }
                _ => Err(corrupt("invalid retained manifest identity")),
            }
        }).transpose()
    }

    /// Keyset-page the canonical references, including generations evicted from
    /// the bounded head's history. The head is not an inventory of SQL liveness.
    pub fn generation_page(
        &self,
        worktree: &tracedecay_domain::WorktreeId,
        after: Option<&tracedecay_domain::CodeGenerationId>,
    ) -> CodeIndexStorageResult<CodeIndexGenerationPage> {
        let snapshot = self.retained.handle()
            .begin_read_snapshot(Duration::from_secs(5)).map_err(sql_error)?;
        let extent = one_row(snapshot.query(statement(
            "SELECT COALESCE(SUM(length(CAST(generation_id AS BLOB))
                + length(CAST(manifest_digest AS BLOB))), 0) FROM (
                SELECT generation_id, manifest_digest FROM code_index_generations
                WHERE worktree_id = ?1 AND (?2 IS NULL OR generation_id > ?2)
                ORDER BY generation_id LIMIT 32)",
            vec![text(worktree.as_str()), after.map_or(ExactSqlValue::Null, |id| text(id.as_str()))],
        )?).map_err(sql_error)?)?.ok_or_else(|| corrupt("missing generation page extent"))?;
        let bytes = match extent.values.as_slice() {
            [ExactSqlValue::Integer(bytes)] => usize::try_from(*bytes)
                .map_err(|_| corrupt("invalid generation page extent"))?,
            _ => return Err(corrupt("invalid generation page extent")),
        };
        let reservation = self.admission.reserve(allocation_cost(bytes, bytes)?)?;
        let rows = snapshot.query(statement(
            "SELECT generation_id, manifest_digest, manifest_size FROM code_index_generations
             WHERE worktree_id = ?1 AND (?2 IS NULL OR generation_id > ?2)
             ORDER BY generation_id LIMIT 32",
            vec![text(worktree.as_str()), after.map_or(ExactSqlValue::Null, |id| text(id.as_str()))],
        )?).map_err(sql_error)?;
        let records = rows.rows.into_iter().map(|row| {
            let mut values = row.values.into_iter();
            match (values.next(), values.next(), values.next(), values.next()) {
                (Some(ExactSqlValue::Text(generation)), Some(ExactSqlValue::Text(digest)),
                    Some(ExactSqlValue::Integer(size)), None) => {
                    Ok(CodeIndexGenerationRecord {
                        key: CodeIndexGenerationKey {
                            worktree_id: worktree.clone(),
                            generation_id: tracedecay_domain::CodeGenerationId::new(generation)
                                .map_err(|error| corrupt(error.to_string()))?,
                        },
                        manifest: CodeIndexPayloadIdentity {
                            digest: ManifestDigest::new(digest)
                                .map_err(|error| corrupt(error.to_string()))?,
                            size_bytes: u64::try_from(size)
                                .map_err(|_| corrupt("negative stored manifest extent"))?,
                        },
                    })
                }
                _ => Err(corrupt("invalid generation inventory row")),
            }
        }).collect::<CodeIndexStorageResult<Vec<_>>>()?;
        Ok(CodeIndexGenerationPage::from_reserved(records, reservation))
    }

    pub fn read_head(
        &self,
        worktree: &tracedecay_domain::WorktreeId,
    ) -> CodeIndexStorageResult<Option<CodeIndexStoredBytes>> {
        let reservation = self.admission.reserve(allocation_cost(MAX_PUBLICATION_HEAD_BYTES, 0)?)?;
        let rows = self.retained.handle().query(statement(
            "SELECT CASE WHEN length(head) <= 524288 THEN head END
             FROM code_index_heads WHERE worktree_id = ?1",
            vec![text(worktree.as_str())],
        )?, Duration::from_secs(5)).map_err(sql_error)?;
        one_row(rows)?.map(|row| {
            single_blob(row).map(|bytes| CodeIndexStoredBytes::from_reserved(bytes, reservation))
        }).transpose()
    }

    /// Reset only the adopted worktree publication head. Immutable references,
    /// graph replay, project memory, diagnostics, and receipts remain intact.
    pub fn reset_head(
        &self,
        worktree: &tracedecay_domain::WorktreeId,
        expected: &[u8],
    ) -> CodeIndexStorageResult<bool> {
        let _reservation = self.admission.reserve(allocation_cost(expected.len(), 0)?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let removed = transaction.execute(statement(
            "DELETE FROM code_index_heads WHERE worktree_id = ?1 AND head = ?2",
            vec![text(worktree.as_str()), ExactSqlValue::Blob(expected.to_vec())],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(removed.changed_rows == 1)
    }

    pub fn read_manifest(
        &self,
        generation: &CodeIndexGenerationKey,
        expected_digest: &ManifestDigest,
    ) -> CodeIndexStorageResult<CodeIndexStoredBytes> {
        let rows = self.retained.handle().query(statement(
            "SELECT manifest_size FROM code_index_generations
             WHERE worktree_id = ?1 AND generation_id = ?2 AND manifest_digest = ?3",
            vec![
                text(generation.worktree_id.as_str()), text(generation.generation_id.as_str()),
                text(expected_digest.as_str()),
            ],
        )?, Duration::from_secs(5)).map_err(sql_error)?;
        let row = match one_row(rows)? {
            Some(row) => row,
            None => {
                self.require_not_retired(generation)?;
                return Err(corrupt("generation has no exact manifest"));
            }
        };
        let mut values = row.values.into_iter();
        let size = match (values.next(), values.next()) {
            (Some(ExactSqlValue::Integer(size)), None) => u64::try_from(size)
                .map_err(|_| corrupt("negative manifest size"))?,
            _ => return Err(corrupt("invalid manifest size")),
        };
        let payload = CodeIndexPayloadIdentity { digest: expected_digest.clone(), size_bytes: size };
        let bytes = self.read_range(generation, &payload, 0, size)?;
        let actual = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes.as_bytes()))
            .map_err(|error| corrupt(error.to_string()))?;
        if actual != *expected_digest {
            return Err(corrupt("manifest encoded digest mismatch"));
        }
        Ok(bytes)
    }

    /// Called only inside the maintained outer blocking boundary. This adapter
    /// neither opens a database nor constructs an async bridge or reader pool.
    pub fn begin_stage(&self, stage: &CodeIndexPublicationStage) -> CodeIndexStorageResult<()> {
        if stage.expected_head.as_ref().is_some_and(|head| head.len() > MAX_PUBLICATION_HEAD_BYTES) {
            return Err(CodeIndexStorageError::InvalidRequest("publication head is oversized".to_owned()));
        }
        let _reservation = self.admission.reserve(allocation_cost(
            stage.expected_head.as_ref().map_or(0, |bytes| bytes.len()), 0,
        )?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let existing = transaction.query(statement(
            "SELECT worktree_id, generation_id, expected_head, authority_owner
             FROM code_index_staging WHERE operation_id = ?1",
            vec![text(stage.operation_id.as_str())],
        )?).map_err(sql_error)?;
        if let Some(row) = one_row(existing)? {
            if row.values != vec![
                text(stage.generation.worktree_id.as_str()),
                text(stage.generation.generation_id.as_str()),
                optional_blob(stage.expected_head.as_ref().map(|bytes| bytes.as_bytes())),
                text(&self.authority_owner),
            ] {
                return Err(CodeIndexStorageError::StagingConflict);
            }
            transaction.rollback().map_err(sql_error)?;
            return Ok(());
        }
        transaction.execute(statement(
            "INSERT INTO code_index_staging
             (operation_id, worktree_id, generation_id, expected_head, authority_owner)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![
                text(stage.operation_id.as_str()),
                text(stage.generation.worktree_id.as_str()),
                text(stage.generation.generation_id.as_str()),
                optional_blob(stage.expected_head.as_ref().map(|bytes| bytes.as_bytes())),
                text(&self.authority_owner),
            ],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// Protect the shared identity before staging any of its regions.
    pub fn stage_payload(
        &self,
        operation: &StoreOperationIdV1,
        payload: &CodeIndexPayloadIdentity,
    ) -> CodeIndexStorageResult<()> {
        let size = sql_u64(payload.size_bytes)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        require_stage(&transaction, operation, &self.authority_owner)?;
        let existing = transaction.query(statement(
            "SELECT size_bytes, sealed FROM code_index_payloads WHERE digest = ?1",
            vec![text(payload.digest.as_str())],
        )?).map_err(sql_error)?;
        match one_row(existing)? {
            Some(row) if matches!(row.values.as_slice(),
                [ExactSqlValue::Integer(actual), ExactSqlValue::Integer(0 | 1)] if *actual == size) => {}
            Some(row) if matches!(row.values.as_slice(),
                [ExactSqlValue::Integer(actual), ExactSqlValue::Integer(2)] if *actual == size) => {
                    return Err(CodeIndexStorageError::ReclamationPending);
                }
            Some(_) => return Err(corrupt("shared digest has a different encoded length")),
            None => {
                transaction.execute(statement(
                    "INSERT INTO code_index_payloads (digest, size_bytes, sealed) VALUES (?1, ?2, 0)",
                    vec![text(payload.digest.as_str()), ExactSqlValue::Integer(size)],
                )?).map_err(sql_error)?;
            }
        }
        transaction.execute(statement(
            "INSERT INTO code_index_staging_refs (operation_id, digest) VALUES (?1, ?2)
             ON CONFLICT(operation_id, digest) DO NOTHING",
            vec![text(operation.as_str()), text(payload.digest.as_str())],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// The codec supplies the aggregate digest only after its final evidence
    /// page. Until then operation-owned bounded pages are staging, not a second
    /// authoritative payload identity.
    pub fn stage_evidence_page(
        &self,
        operation: &StoreOperationIdV1,
        ordinal: u64,
        digest: &ManifestDigest,
        bytes: &[u8],
    ) -> CodeIndexStorageResult<()> {
        if bytes.is_empty() || bytes.len() > CODE_INDEX_REGION_BYTES {
            return Err(CodeIndexStorageError::InvalidRequest("invalid evidence page extent".to_owned()));
        }
        let actual = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
            .map_err(|error| corrupt(error.to_string()))?;
        if actual != *digest {
            return Err(corrupt("staged evidence page digest mismatch"));
        }
        let _reservation = self.admission.reserve(allocation_cost(bytes.len(), bytes.len())?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        require_stage(&transaction, operation, &self.authority_owner)?;
        let existing = transaction.query(statement(
            "SELECT digest, CASE WHEN length(body) <= 1048576 THEN body END
             FROM code_index_staging_pages WHERE operation_id = ?1 AND ordinal = ?2",
            vec![text(operation.as_str()), ExactSqlValue::Integer(sql_u64(ordinal)?)],
        )?).map_err(sql_error)?;
        if let Some(row) = one_row(existing)? {
            if row.values.len() != 2
                || row.values[0] != text(digest.as_str())
                || !matches!(&row.values[1], ExactSqlValue::Blob(body) if body == bytes)
            {
                return Err(corrupt("staged evidence page differs from its original bytes"));
            }
            transaction.rollback().map_err(sql_error)?;
            return Ok(());
        }
        transaction.execute(statement(
            "INSERT INTO code_index_staging_pages (operation_id, ordinal, digest, body)
             VALUES (?1, ?2, ?3, ?4)",
            vec![
                text(operation.as_str()), ExactSqlValue::Integer(sql_u64(ordinal)?),
                text(digest.as_str()), ExactSqlValue::Blob(bytes.to_vec()),
            ],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// Bind the bounded page stream to the codec's unchanged final encoded SHA.
    /// CPU, hashing, and at most one region of assembly stay outside the writer.
    pub fn commit_evidence_pages(
        &self,
        operation: &StoreOperationIdV1,
        payload: &CodeIndexPayloadIdentity,
        pages: u64,
    ) -> CodeIndexStorageResult<()> {
        self.stage_payload(operation, payload)?;
        let _reservation = self.admission.reserve(allocation_cost(CODE_INDEX_REGION_BYTES, CODE_INDEX_REGION_BYTES)?)?;
        let mut region = Vec::new();
        region.try_reserve_exact(CODE_INDEX_REGION_BYTES)
            .map_err(|error| CodeIndexStorageError::AllocationRefused(error.to_string()))?;
        let mut hasher = Sha256::new();
        let mut ordinal = 0_u64;
        let mut total = 0_u64;
        for page in 0..pages {
            let rows = self.retained.handle().query(statement(
                "SELECT digest, CASE WHEN length(body) <= 1048576 THEN body END
                 FROM code_index_staging_pages WHERE operation_id = ?1 AND ordinal = ?2",
                vec![text(operation.as_str()), ExactSqlValue::Integer(sql_u64(page)?)],
            )?, Duration::from_secs(5)).map_err(sql_error)?;
            let row = one_row(rows)?.ok_or_else(|| corrupt("missing staged evidence page"))?;
            let mut values = row.values.into_iter();
            let (digest, body) = match (values.next(), values.next(), values.next()) {
                (Some(ExactSqlValue::Text(digest)), Some(ExactSqlValue::Blob(body)), None) => (digest, body),
                _ => return Err(corrupt("invalid staged evidence page")),
            };
            let actual = ManifestDigest::from_sha256_bytes(&Sha256::digest(&body))
                .map_err(|error| corrupt(error.to_string()))?;
            if actual.as_str() != digest {
                return Err(corrupt("staged evidence page no longer matches its digest"));
            }
            total = total.checked_add(body.len() as u64)
                .ok_or_else(|| corrupt("evidence stream length overflow"))?;
            hasher.update(&body);
            let mut remainder = body.as_slice();
            while !remainder.is_empty() {
                let amount = remainder.len().min(CODE_INDEX_REGION_BYTES - region.len());
                region.extend_from_slice(&remainder[..amount]);
                remainder = &remainder[amount..];
                if region.len() == CODE_INDEX_REGION_BYTES {
                    self.put_region(operation, payload, ordinal, &region)?;
                    ordinal += 1;
                    region.clear();
                }
            }
        }
        if !region.is_empty() {
            self.put_region(operation, payload, ordinal, &region)?;
        }
        let actual = ManifestDigest::from_sha256_bytes(&hasher.finalize())
            .map_err(|error| corrupt(error.to_string()))?;
        if total != payload.size_bytes || actual != payload.digest {
            return Err(corrupt("evidence aggregate identity differs from its codec commitment"));
        }
        self.seal_payload(operation, payload)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let count = one_row(transaction.query(statement(
            "SELECT COUNT(*) FROM code_index_staging_pages WHERE operation_id = ?1",
            vec![text(operation.as_str())],
        )?).map_err(sql_error)?)?.ok_or_else(|| corrupt("missing staged page inventory"))?;
        if count.values != vec![ExactSqlValue::Integer(sql_u64(pages)?)] {
            return Err(corrupt("evidence page inventory differs from its commit"));
        }
        transaction.rollback().map_err(sql_error)?;
        for page in 0..pages {
            let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
            require_stage(&transaction, operation, &self.authority_owner)?;
            transaction.execute(statement(
                "DELETE FROM code_index_staging_pages WHERE operation_id = ?1 AND ordinal = ?2",
                vec![text(operation.as_str()), ExactSqlValue::Integer(sql_u64(page)?)],
            )?).map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
        }
        Ok(())
    }

    /// One bounded transaction per region. Existing digest rows are compared
    /// byte-for-byte; conflict-ignore never authenticates an existing payload.
    pub fn put_region(
        &self,
        operation: &StoreOperationIdV1,
        payload: &CodeIndexPayloadIdentity,
        ordinal: u64,
        bytes: &[u8],
    ) -> CodeIndexStorageResult<()> {
        if bytes.len() != region_length(payload.size_bytes, ordinal)? {
            return Err(CodeIndexStorageError::InvalidRequest("incorrect region extent".to_owned()));
        }
        let _reservation = self.admission.reserve(allocation_cost(bytes.len(), bytes.len())?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        require_staged_payload(&transaction, operation, payload, &self.authority_owner)?;
        let existing = transaction.query(statement(
            "SELECT CASE WHEN length(body) <= 1048576 THEN body END
             FROM code_index_regions WHERE digest = ?1 AND ordinal = ?2",
            vec![text(payload.digest.as_str()), ExactSqlValue::Integer(sql_u64(ordinal)?)],
        )?).map_err(sql_error)?;
        if let Some(row) = one_row(existing)? {
            if row.values.len() != 1
                || !matches!(&row.values[0], ExactSqlValue::Blob(body) if body == bytes)
            {
                return Err(corrupt("shared encoded region differs from its staged bytes"));
            }
            transaction.rollback().map_err(sql_error)?;
            return Ok(());
        }
        transaction.execute(statement(
            "INSERT INTO code_index_regions (digest, ordinal, body) VALUES (?1, ?2, ?3)",
            vec![
                text(payload.digest.as_str()),
                ExactSqlValue::Integer(sql_u64(ordinal)?),
                ExactSqlValue::Blob(bytes.to_vec()),
            ],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// Hash the original encoded byte stream outside the sole SQL writer.
    /// Staging references prevent collection while this bounded scan runs.
    pub fn seal_payload(
        &self,
        operation: &StoreOperationIdV1,
        payload: &CodeIndexPayloadIdentity,
    ) -> CodeIndexStorageResult<()> {
        let mut hasher = Sha256::new();
        for ordinal in 0..region_count(payload.size_bytes) {
            let length = region_length(payload.size_bytes, ordinal)?;
            let _reservation = self.admission.reserve(allocation_cost(length, 0)?)?;
            let rows = self.retained.handle().query(statement(
                "SELECT CASE WHEN length(r.body) <= 1048576 THEN r.body END FROM code_index_regions r
                 JOIN code_index_staging_refs s ON s.digest = r.digest
                 JOIN code_index_payloads p ON p.digest = r.digest
                 WHERE s.operation_id = ?1 AND r.digest = ?2 AND r.ordinal = ?3
                     AND p.size_bytes = ?4",
                vec![
                    text(operation.as_str()), text(payload.digest.as_str()),
                    ExactSqlValue::Integer(sql_u64(ordinal)?),
                    ExactSqlValue::Integer(sql_u64(payload.size_bytes)?),
                ],
            )?, Duration::from_secs(5)).map_err(sql_error)?;
            let body = region_body(rows, length)?;
            hasher.update(&body);
        }
        let actual = ManifestDigest::from_sha256_bytes(&hasher.finalize())
            .map_err(|error| corrupt(error.to_string()))?;
        if actual != payload.digest {
            return Err(corrupt("encoded payload digest mismatch"));
        }
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        require_staged_payload(&transaction, operation, payload, &self.authority_owner)?;
        require_complete_payload(&transaction, payload)?;
        transaction.execute(statement(
            "UPDATE code_index_payloads SET sealed = 1 WHERE digest = ?1",
            vec![text(payload.digest.as_str())],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// The complete manifest is itself a canonical payload. Publication installs
    /// references and exact incumbent-head CAS in the same short transaction.
    pub fn publish(
        &self,
        stage: &CodeIndexPublicationStage,
        manifest: &CodeIndexPayloadIdentity,
        head: &[u8],
    ) -> CodeIndexStorageResult<CodeIndexPublishOutcome> {
        self.publish_head(stage, manifest, head, &stage.generation.generation_id)
    }

    pub fn publish_head(
        &self,
        stage: &CodeIndexPublicationStage,
        manifest: &CodeIndexPayloadIdentity,
        head: &[u8],
        head_generation: &tracedecay_domain::CodeGenerationId,
    ) -> CodeIndexStorageResult<CodeIndexPublishOutcome> {
        if head.is_empty() || head.len() > MAX_PUBLICATION_HEAD_BYTES {
            return Err(CodeIndexStorageError::InvalidRequest("invalid publication head length".to_owned()));
        }
        let _reservation = self.admission.reserve(allocation_cost(head.len(), head.len())?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let existing = transaction.query(statement(
            "SELECT worktree_id, generation_id, manifest_digest, manifest_size, publication_head
             FROM code_index_generations WHERE operation_id = ?1",
            vec![text(stage.operation_id.as_str())],
        )?).map_err(sql_error)?;
        if let Some(row) = one_row(existing)? {
            if row.values != vec![
                text(stage.generation.worktree_id.as_str()),
                text(stage.generation.generation_id.as_str()),
                text(manifest.digest.as_str()),
                ExactSqlValue::Integer(sql_u64(manifest.size_bytes)?),
                ExactSqlValue::Blob(head.to_vec()),
            ] {
                return Err(CodeIndexStorageError::StagingConflict);
            }
            transaction.rollback().map_err(sql_error)?;
            return Ok(CodeIndexPublishOutcome::AlreadyPublished);
        }
        let recorded = transaction.query(statement(
            "SELECT worktree_id, generation_id, expected_head
             FROM code_index_staging WHERE operation_id = ?1",
            vec![text(stage.operation_id.as_str())],
        )?).map_err(sql_error)?;
        let recorded = one_row(recorded)?.ok_or(CodeIndexStorageError::StagingConflict)?;
        if recorded.values != vec![
            text(stage.generation.worktree_id.as_str()),
            text(stage.generation.generation_id.as_str()),
            optional_blob(stage.expected_head.as_ref().map(|bytes| bytes.as_bytes())),
        ] {
            return Err(CodeIndexStorageError::StagingConflict);
        }
        let incumbent = transaction.query(statement(
            "SELECT head FROM code_index_heads WHERE worktree_id = ?1",
            vec![text(stage.generation.worktree_id.as_str())],
        )?).map_err(sql_error)?;
        let actual = one_row(incumbent)?
            .map(single_blob).transpose()?;
        if actual.as_deref() != stage.expected_head.as_ref().map(|bytes| bytes.as_bytes()) {
            transaction.rollback().map_err(sql_error)?;
            return Ok(CodeIndexPublishOutcome::HeadConflict);
        }
        require_staged_payload(&transaction, &stage.operation_id, manifest, &self.authority_owner)?;
        let invalid = transaction.query(statement(
            "SELECT 1 FROM code_index_staging_refs s
             JOIN code_index_payloads p ON p.digest = s.digest
             WHERE s.operation_id = ?1 AND (
                 p.sealed != 1
                 OR p.stored_bytes != p.size_bytes
                 OR p.region_count != (p.size_bytes / 1048576 + (p.size_bytes % 1048576 != 0))
             ) LIMIT 1",
            vec![text(stage.operation_id.as_str())],
        )?).map_err(sql_error)?;
        if !invalid.rows.is_empty() {
            return Err(corrupt("publication contains incomplete or unsealed payloads"));
        }
        transaction.execute(statement(
            "INSERT INTO code_index_generations
             (worktree_id, generation_id, operation_id, manifest_digest, manifest_size, publication_head)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            vec![
                text(stage.generation.worktree_id.as_str()),
                text(stage.generation.generation_id.as_str()),
                text(stage.operation_id.as_str()), text(manifest.digest.as_str()),
                ExactSqlValue::Integer(sql_u64(manifest.size_bytes)?),
                ExactSqlValue::Blob(head.to_vec()),
            ],
        )?).map_err(sql_error)?;
        transaction.execute(statement(
            "INSERT INTO code_index_generation_refs (worktree_id, generation_id, digest)
             SELECT ?1, ?2, digest FROM code_index_staging_refs WHERE operation_id = ?3",
            vec![
                text(stage.generation.worktree_id.as_str()),
                text(stage.generation.generation_id.as_str()), text(stage.operation_id.as_str()),
            ],
        )?).map_err(sql_error)?;
        transaction.execute(statement(
            "INSERT INTO code_index_heads (worktree_id, generation_id, head) VALUES (?1, ?2, ?3)
             ON CONFLICT(worktree_id) DO UPDATE SET
                 generation_id = excluded.generation_id, head = excluded.head",
            vec![
                text(stage.generation.worktree_id.as_str()),
                text(head_generation.as_str()), ExactSqlValue::Blob(head.to_vec()),
            ],
        )?).map_err(sql_error)?;
        delete_stage(&transaction, &stage.operation_id)?;
        transaction.commit().map_err(sql_error)?;
        Ok(CodeIndexPublishOutcome::Published)
    }

    /// Derived descriptor updates use the same exact incumbent bytes as the
    /// publication CAS, never a second JSON visibility boundary.
    pub fn compare_and_swap_head(
        &self,
        generation: &CodeIndexGenerationKey,
        expected: &[u8],
        next: &[u8],
    ) -> CodeIndexStorageResult<bool> {
        if next.is_empty() || next.len() > MAX_PUBLICATION_HEAD_BYTES {
            return Err(CodeIndexStorageError::InvalidRequest("invalid publication head length".to_owned()));
        }
        let _reservation = self.admission.reserve(allocation_cost(expected.len() + next.len(), next.len())?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let changed = transaction.execute(statement(
            "UPDATE code_index_heads SET head = ?3
             WHERE worktree_id = ?1 AND generation_id = ?2 AND head = ?4",
            vec![
                text(generation.worktree_id.as_str()), text(generation.generation_id.as_str()),
                ExactSqlValue::Blob(next.to_vec()), ExactSqlValue::Blob(expected.to_vec()),
            ],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(changed.changed_rows == 1)
    }

    pub fn stage_existing_reference(
        &self,
        operation: &StoreOperationIdV1,
        payload: &CodeIndexPayloadIdentity,
    ) -> CodeIndexStorageResult<()> {
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        require_stage(&transaction, operation, &self.authority_owner)?;
        let row = one_row(transaction.query(statement(
            "SELECT size_bytes FROM code_index_payloads WHERE digest = ?1 AND sealed = 1",
            vec![text(payload.digest.as_str())],
        )?).map_err(sql_error)?)?.ok_or_else(|| corrupt("referenced payload is not sealed"))?;
        if row.values != vec![ExactSqlValue::Integer(sql_u64(payload.size_bytes)?)] {
            return Err(corrupt("referenced payload has a different encoded extent"));
        }
        transaction.execute(statement(
            "INSERT INTO code_index_staging_refs (operation_id, digest) VALUES (?1, ?2)
             ON CONFLICT(operation_id, digest) DO NOTHING",
            vec![text(operation.as_str()), text(payload.digest.as_str())],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    pub fn read_range(
        &self,
        generation: &CodeIndexGenerationKey,
        payload: &CodeIndexPayloadIdentity,
        offset: u64,
        length: u64,
    ) -> CodeIndexStorageResult<CodeIndexStoredBytes> {
        let end = offset.checked_add(length)
            .filter(|end| *end <= payload.size_bytes)
            .ok_or_else(|| CodeIndexStorageError::InvalidRequest("range exceeds payload".to_owned()))?;
        let output_len = usize::try_from(length)
            .map_err(|_| CodeIndexStorageError::InvalidRequest("range exceeds address space".to_owned()))?;
        let payload_size = sql_u64(payload.size_bytes)?;
        let reservation = self.admission.reserve(allocation_cost(CODE_INDEX_REGION_BYTES, output_len)?)?;
        let reference = self.retained.handle().query(statement(
            "SELECT p.size_bytes FROM code_index_generation_refs g
             JOIN code_index_payloads p ON p.digest = g.digest
             WHERE g.worktree_id = ?1 AND g.generation_id = ?2 AND g.digest = ?3 AND p.sealed = 1",
            vec![
                text(generation.worktree_id.as_str()), text(generation.generation_id.as_str()),
                text(payload.digest.as_str()),
            ],
        )?, Duration::from_secs(5)).map_err(sql_error)?;
        if one_row(reference)?.is_none_or(|row| row.values != vec![
            ExactSqlValue::Integer(payload_size),
        ]) {
            self.require_not_retired(generation)?;
            return Err(corrupt("generation does not retain the exact payload"));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(output_len).map_err(|error| {
            CodeIndexStorageError::AllocationRefused(error.to_string())
        })?;
        if length > 0 {
            let first = offset / CODE_INDEX_REGION_BYTES as u64;
            let last = (end - 1) / CODE_INDEX_REGION_BYTES as u64;
            for ordinal in first..=last {
                let rows = self.retained.handle().query(statement(
                    "SELECT CASE WHEN length(r.body) <= 1048576 THEN r.body END
                     FROM code_index_regions r
                     JOIN code_index_generation_refs g ON g.digest = r.digest
                     WHERE g.worktree_id = ?1 AND g.generation_id = ?2
                         AND r.digest = ?3 AND r.ordinal = ?4",
                    vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str()),
                        text(payload.digest.as_str()), ExactSqlValue::Integer(sql_u64(ordinal)?)],
                )?, Duration::from_secs(5)).map_err(sql_error)?;
                if rows.rows.is_empty() {
                    self.require_not_retired(generation)?;
                    return Err(corrupt("live generation lost an encoded region"));
                }
                let region = region_body(rows, region_length(payload.size_bytes, ordinal)?)?;
                let base = ordinal * CODE_INDEX_REGION_BYTES as u64;
                let start = usize::try_from(offset.saturating_sub(base))
                    .map_err(|_| corrupt("region offset overflows address space"))?;
                let stop = usize::try_from((end - base).min(region.len() as u64))
                    .map_err(|_| corrupt("region end overflows address space"))?;
                bytes.extend_from_slice(&region[start..stop]);
            }
        }
        if bytes.len() != output_len {
            return Err(corrupt("range materialized an incorrect byte count"));
        }
        Ok(CodeIndexStoredBytes::from_reserved(bytes, reservation))
    }

    /// Authenticate the original encoded stream without a payload-sized result.
    /// Each region is charged and released before the next short SQL snapshot.
    pub fn verify_payload(
        &self,
        generation: &CodeIndexGenerationKey,
        payload: &CodeIndexPayloadIdentity,
        check: &dyn Fn() -> CodeIndexStorageResult<()>,
    ) -> CodeIndexStorageResult<()> {
        let mut hasher = Sha256::new();
        for ordinal in 0..region_count(payload.size_bytes) {
            check()?;
            let offset = ordinal.checked_mul(CODE_INDEX_REGION_BYTES as u64)
                .ok_or_else(|| corrupt("payload region offset overflow"))?;
            let bytes = self.read_range(
                generation, payload, offset, region_length(payload.size_bytes, ordinal)? as u64,
            )?;
            hasher.update(bytes.as_bytes());
        }
        check()?;
        let actual = ManifestDigest::from_sha256_bytes(&hasher.finalize())
            .map_err(|error| corrupt(error.to_string()))?;
        if actual != payload.digest {
            return Err(corrupt("retained encoded payload digest mismatch"));
        }
        Ok(())
    }

    fn require_not_retired(&self, generation: &CodeIndexGenerationKey) -> CodeIndexStorageResult<()> {
        let rows = self.retained.handle().query(statement(
            "SELECT 1 FROM code_index_retirement_receipts
             WHERE worktree_id = ?1 AND generation_id = ?2 AND state = 'complete' LIMIT 1",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str())],
        )?, Duration::from_secs(5)).map_err(sql_error)?;
        if !rows.rows.is_empty() {
            return Err(CodeIndexStorageError::Retired);
        }
        Ok(())
    }

    /// Remove candidate visibility and record its receipt before graph cleanup.
    /// The payload remains readable until completion. Repeating the same intent
    /// is idempotent; a changed incumbent never permits a stale preparation.
    pub fn prepare_retirement(
        &self,
        generation: &CodeIndexGenerationKey,
        operation: &StoreOperationIdV1,
        manifest: &ManifestDigest,
        receipt: &[u8],
        expected_head: &[u8],
        next_head: &[u8],
    ) -> CodeIndexStorageResult<()> {
        if receipt.is_empty() || receipt.len() > CODE_INDEX_REGION_BYTES
            || next_head.is_empty() || next_head.len() > MAX_PUBLICATION_HEAD_BYTES
        {
            return Err(CodeIndexStorageError::InvalidRequest("invalid retirement intent".to_owned()));
        }
        let _reservation = self.admission.reserve(allocation_cost(
            receipt.len() + expected_head.len() + next_head.len(), next_head.len(),
        )?)?;
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let existing = transaction.query(statement(
            "SELECT operation_id, manifest_digest, receipt FROM code_index_retirement_receipts
             WHERE worktree_id = ?1 AND generation_id = ?2",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str())],
        )?).map_err(sql_error)?;
        if let Some(row) = one_row(existing)? {
            if row.values != vec![text(operation.as_str()), text(manifest.as_str()),
                ExactSqlValue::Blob(receipt.to_vec())] {
                return Err(CodeIndexStorageError::StagingConflict);
            }
            transaction.rollback().map_err(sql_error)?;
            return Ok(());
        }
        let current = transaction.query(statement(
            "SELECT 1 FROM code_index_heads WHERE worktree_id = ?1 AND generation_id = ?2",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str())],
        )?).map_err(sql_error)?;
        if !current.rows.is_empty() {
            return Err(CodeIndexStorageError::Retained);
        }
        let changed = transaction.execute(statement(
            "UPDATE code_index_heads SET head = ?2 WHERE worktree_id = ?1 AND head = ?3",
            vec![text(generation.worktree_id.as_str()), ExactSqlValue::Blob(next_head.to_vec()),
                ExactSqlValue::Blob(expected_head.to_vec())],
        )?).map_err(sql_error)?;
        if changed.changed_rows != 1 {
            return Err(CodeIndexStorageError::StagingConflict);
        }
        let inserted = transaction.execute(statement(
            "INSERT INTO code_index_retirement_receipts
             (operation_id, worktree_id, generation_id, manifest_digest, state, receipt)
             SELECT ?1, worktree_id, generation_id, manifest_digest, 'pending', ?4
             FROM code_index_generations
             WHERE worktree_id = ?2 AND generation_id = ?3 AND manifest_digest = ?5",
            vec![text(operation.as_str()), text(generation.worktree_id.as_str()),
                text(generation.generation_id.as_str()), ExactSqlValue::Blob(receipt.to_vec()),
                text(manifest.as_str())],
        )?).map_err(sql_error)?;
        if inserted.changed_rows != 1 {
            return Err(corrupt("retirement intent has no exact stored manifest"));
        }
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    pub fn pending_retirement(
        &self,
        generation: &CodeIndexGenerationKey,
    ) -> CodeIndexStorageResult<Option<(StoreOperationIdV1, CodeIndexStoredBytes)>> {
        let reservation = self.admission.reserve(allocation_cost(CODE_INDEX_REGION_BYTES, 0)?)?;
        let row = one_row(self.retained.handle().query(statement(
            "SELECT operation_id, CASE WHEN length(receipt) <= 1048576 THEN receipt END
             FROM code_index_retirement_receipts
             WHERE worktree_id = ?1 AND generation_id = ?2 AND state = 'pending'",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str())],
        )?, Duration::from_secs(5)).map_err(sql_error)?)?;
        row.map(|row| {
            let mut values = row.values.into_iter();
            match (values.next(), values.next(), values.next()) {
                (Some(ExactSqlValue::Text(operation)), Some(ExactSqlValue::Blob(bytes)), None) => {
                    Ok((StoreOperationIdV1::new(operation)
                        .map_err(|error| corrupt(error.to_string()))?,
                        CodeIndexStoredBytes::from_reserved(bytes, reservation)))
                }
                _ => Err(corrupt("invalid pending retirement receipt")),
            }
        }).transpose()
    }

    pub fn complete_retirement(
        &self,
        generation: &CodeIndexGenerationKey,
        operation: &StoreOperationIdV1,
    ) -> CodeIndexStorageResult<()> {
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let intent = one_row(transaction.query(statement(
            "SELECT state FROM code_index_retirement_receipts
             WHERE operation_id = ?1 AND worktree_id = ?2 AND generation_id = ?3",
            vec![text(operation.as_str()), text(generation.worktree_id.as_str()),
                text(generation.generation_id.as_str())],
        )?).map_err(sql_error)?)?.ok_or(CodeIndexStorageError::StagingConflict)?;
        if intent.values == vec![text("complete")] {
            transaction.rollback().map_err(sql_error)?;
            return Ok(());
        }
        if intent.values != vec![text("pending")] {
            return Err(corrupt("invalid retirement intent state"));
        }
        let retained = transaction.query(statement(
            "SELECT 1 FROM code_index_heads WHERE worktree_id = ?1 AND generation_id = ?2
             UNION ALL
             SELECT 1 FROM graph_publication_replay_v1 r
             JOIN code_index_retirement_receipts i
                 ON i.manifest_digest = json_extract(CAST(r.canonical_replay_source AS TEXT),
                    '$.sealed_code_generation.sealed_state_digest')
                 AND i.generation_id = json_extract(CAST(r.canonical_replay_source AS TEXT),
                    '$.sealed_code_generation.generation')
             WHERE i.operation_id = ?3 LIMIT 1",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str()),
                text(operation.as_str())],
        )?).map_err(sql_error)?;
        if !retained.rows.is_empty() {
            return Err(CodeIndexStorageError::Retained);
        }
        let removed = transaction.execute(statement(
            "DELETE FROM code_index_generations WHERE worktree_id = ?1 AND generation_id = ?2",
            vec![text(generation.worktree_id.as_str()), text(generation.generation_id.as_str())],
        )?).map_err(sql_error)?;
        if removed.changed_rows != 1 {
            return Err(corrupt("pending retirement lost its exact generation"));
        }
        transaction.execute(statement(
            "UPDATE code_index_retirement_receipts SET state = 'complete' WHERE operation_id = ?1",
            vec![text(operation.as_str())],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(())
    }

    /// One bounded region reclaimed per transaction after canonical retirement.
    /// No live generation or operation can lose its shared payload.
    pub fn collect_unreferenced_payload(&self) -> CodeIndexStorageResult<bool> {
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let candidate = transaction.query(statement(
            "SELECT p.digest FROM code_index_payloads p
             WHERE NOT EXISTS (SELECT 1 FROM code_index_generation_refs g WHERE g.digest = p.digest)
                 AND NOT EXISTS (SELECT 1 FROM code_index_staging_refs s WHERE s.digest = p.digest)
             ORDER BY p.digest LIMIT 1",
            vec![],
        )?).map_err(sql_error)?;
        let Some(row) = one_row(candidate)? else {
            transaction.rollback().map_err(sql_error)?;
            return Ok(false);
        };
        let mut values = row.values.into_iter();
        let digest = match (values.next(), values.next()) {
            (Some(ExactSqlValue::Text(digest)), None) => digest,
            _ => return Err(corrupt("invalid collection candidate")),
        };
        transaction.execute(statement(
            "UPDATE code_index_payloads SET sealed = 2 WHERE digest = ?1",
            vec![text(&digest)],
        )?).map_err(sql_error)?;
        transaction.execute(statement(
            "DELETE FROM code_index_regions WHERE digest = ?1
             AND ordinal = (SELECT MIN(ordinal) FROM code_index_regions WHERE digest = ?1)",
            vec![text(&digest)],
        )?).map_err(sql_error)?;
        transaction.execute(statement(
            "DELETE FROM code_index_payloads WHERE digest = ?1
             AND NOT EXISTS (SELECT 1 FROM code_index_regions WHERE digest = ?1)",
            vec![text(&digest)],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(true)
    }

    /// An adopted operation may abandon only its own unpublished staging.
    pub fn abandon_stage(&self, operation: &StoreOperationIdV1) -> CodeIndexStorageResult<()> {
        loop {
            let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
            let owned = transaction.query(statement(
                "SELECT 1 FROM code_index_staging WHERE operation_id = ?1 AND authority_owner = ?2",
                vec![text(operation.as_str()), text(&self.authority_owner)],
            )?).map_err(sql_error)?;
            if owned.rows.is_empty() {
                transaction.rollback().map_err(sql_error)?;
                return Ok(());
            }
            let removed = transaction.execute(statement(
                "DELETE FROM code_index_staging_pages WHERE operation_id = ?1
                 AND ordinal = (SELECT MIN(ordinal) FROM code_index_staging_pages WHERE operation_id = ?1)",
                vec![text(operation.as_str())],
            )?).map_err(sql_error)?;
            if removed.changed_rows == 1 {
                transaction.commit().map_err(sql_error)?;
                continue;
            }
            let removed = transaction.execute(statement(
                "DELETE FROM code_index_staging_refs WHERE operation_id = ?1
                 AND digest = (SELECT MIN(digest) FROM code_index_staging_refs WHERE operation_id = ?1)",
                vec![text(operation.as_str())],
            )?).map_err(sql_error)?;
            if removed.changed_rows == 1 {
                transaction.commit().map_err(sql_error)?;
                continue;
            }
            delete_stage(&transaction, operation)?;
            transaction.commit().map_err(sql_error)?;
            return Ok(());
        }
    }

    /// Authority tokens are shared by all live clients and reminted only after
    /// that registered owner ends. No active owner's staged references expire.
    pub fn collect_abandoned_stage(&self) -> CodeIndexStorageResult<bool> {
        let transaction = self.retained.handle().begin_immediate().map_err(sql_error)?;
        let candidate = transaction.query(statement(
            "SELECT operation_id FROM code_index_staging WHERE authority_owner != ?1
             ORDER BY operation_id LIMIT 1",
            vec![text(&self.authority_owner)],
        )?).map_err(sql_error)?;
        let Some(candidate) = one_row(candidate)? else {
            transaction.rollback().map_err(sql_error)?;
            return Ok(false);
        };
        let operation = match candidate.values.as_slice() {
            [ExactSqlValue::Text(operation)] => operation,
            _ => return Err(corrupt("invalid abandoned staging owner")),
        };
        let removed = transaction.execute(statement(
            "DELETE FROM code_index_staging_pages WHERE operation_id = ?1
             AND ordinal = (SELECT MIN(ordinal) FROM code_index_staging_pages WHERE operation_id = ?1)",
            vec![text(operation)],
        )?).map_err(sql_error)?;
        if removed.changed_rows == 1 {
            transaction.commit().map_err(sql_error)?;
            return Ok(true);
        }
        let removed = transaction.execute(statement(
            "DELETE FROM code_index_staging_refs WHERE operation_id = ?1
             AND digest = (SELECT MIN(digest) FROM code_index_staging_refs WHERE operation_id = ?1)",
            vec![text(operation)],
        )?).map_err(sql_error)?;
        if removed.changed_rows == 1 {
            transaction.commit().map_err(sql_error)?;
            return Ok(true);
        }
        let removed = transaction.execute(statement(
            "DELETE FROM code_index_staging WHERE operation_id = ?1 AND authority_owner != ?2",
            vec![text(operation), text(&self.authority_owner)],
        )?).map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        Ok(removed.changed_rows == 1)
    }
}

fn delete_stage(
    transaction: &ExactSqlTransaction,
    operation: &StoreOperationIdV1,
) -> CodeIndexStorageResult<()> {
    transaction.execute(statement(
        "DELETE FROM code_index_staging WHERE operation_id = ?1",
        vec![text(operation.as_str())],
    )?).map_err(sql_error)?;
    Ok(())
}

fn require_stage(
    transaction: &ExactSqlTransaction,
    operation: &StoreOperationIdV1,
    authority_owner: &str,
) -> CodeIndexStorageResult<()> {
    if one_row(transaction.query(statement(
        "SELECT 1 FROM code_index_staging WHERE operation_id = ?1 AND authority_owner = ?2",
        vec![text(operation.as_str()), text(authority_owner)],
    )?).map_err(sql_error)?)?.is_none() {
        return Err(CodeIndexStorageError::StagingConflict);
    }
    Ok(())
}

fn require_staged_payload(
    transaction: &ExactSqlTransaction,
    operation: &StoreOperationIdV1,
    payload: &CodeIndexPayloadIdentity,
    authority_owner: &str,
) -> CodeIndexStorageResult<()> {
    require_stage(transaction, operation, authority_owner)?;
    let size = sql_u64(payload.size_bytes)?;
    if one_row(transaction.query(statement(
        "SELECT p.size_bytes FROM code_index_staging_refs s
         JOIN code_index_payloads p ON p.digest = s.digest
         WHERE s.operation_id = ?1 AND s.digest = ?2",
        vec![text(operation.as_str()), text(payload.digest.as_str())],
    )?).map_err(sql_error)?)?
        .is_none_or(|row| row.values != vec![ExactSqlValue::Integer(size)])
    {
        return Err(CodeIndexStorageError::StagingConflict);
    }
    Ok(())
}

fn require_complete_payload(
    transaction: &ExactSqlTransaction,
    payload: &CodeIndexPayloadIdentity,
) -> CodeIndexStorageResult<()> {
    let rows = transaction.query(statement(
        "SELECT region_count, stored_bytes FROM code_index_payloads WHERE digest = ?1",
        vec![text(payload.digest.as_str())],
    )?).map_err(sql_error)?;
    let count = sql_u64(region_count(payload.size_bytes))?;
    let row = one_row(rows)?.ok_or_else(|| corrupt("missing region inventory"))?;
    if row.values != vec![
        ExactSqlValue::Integer(count),
        ExactSqlValue::Integer(sql_u64(payload.size_bytes)?),
    ] {
        return Err(corrupt("payload region inventory is incomplete"));
    }
    Ok(())
}

fn region_count(size: u64) -> u64 {
    size.div_ceil(CODE_INDEX_REGION_BYTES as u64)
}

fn region_length(size: u64, ordinal: u64) -> CodeIndexStorageResult<usize> {
    let start = ordinal.checked_mul(CODE_INDEX_REGION_BYTES as u64)
        .filter(|start| *start < size)
        .ok_or_else(|| CodeIndexStorageError::InvalidRequest("invalid region ordinal".to_owned()))?;
    Ok((size - start).min(CODE_INDEX_REGION_BYTES as u64) as usize)
}

fn region_body(rows: ExactSqlRows, expected: usize) -> CodeIndexStorageResult<Vec<u8>> {
    let body = single_blob(one_row(rows)?.ok_or_else(|| corrupt("missing encoded region"))?)?;
    if body.len() != expected {
        return Err(corrupt("encoded region has an incorrect length"));
    }
    Ok(body)
}

fn single_blob(row: ExactSqlRow) -> CodeIndexStorageResult<Vec<u8>> {
    let mut values = row.values.into_iter();
    match (values.next(), values.next()) {
        (Some(ExactSqlValue::Blob(bytes)), None) => Ok(bytes),
        _ => Err(corrupt("invalid encoded BLOB row")),
    }
}

fn one_row(rows: ExactSqlRows) -> CodeIndexStorageResult<Option<ExactSqlRow>> {
    if rows.rows.len() > 1 {
        return Err(corrupt("unique storage identity returned multiple rows"));
    }
    Ok(rows.rows.into_iter().next())
}

fn allocation_cost(region: usize, output: usize) -> CodeIndexStorageResult<u64> {
    // SQLite value + owned exact-SQL transport + caller output, including the
    // bounded row/column structures and statement parameters.
    region.checked_mul(2).and_then(|bytes| bytes.checked_add(output))
        .and_then(|bytes| bytes.checked_add(8192))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| CodeIndexStorageError::InvalidRequest("allocation size overflow".to_owned()))
}

fn sql_u64(value: u64) -> CodeIndexStorageResult<i64> {
    i64::try_from(value)
        .map_err(|_| CodeIndexStorageError::InvalidRequest("value exceeds SQLite integer".to_owned()))
}

fn text(value: &str) -> ExactSqlValue {
    ExactSqlValue::Text(value.to_owned())
}

fn optional_blob(value: Option<&[u8]>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, |bytes| ExactSqlValue::Blob(bytes.to_vec()))
}

fn statement(sql: &str, params: Vec<ExactSqlValue>) -> CodeIndexStorageResult<ExactSqlStatement> {
    ExactSqlStatement::new(sql.to_owned(), params).map_err(sql_error)
}

fn sql_error(error: ExactSqlError) -> CodeIndexStorageError {
    CodeIndexStorageError::Infrastructure(error.to_string())
}

fn corrupt(message: impl Into<String>) -> CodeIndexStorageError {
    CodeIndexStorageError::Corrupt(message.into())
}
