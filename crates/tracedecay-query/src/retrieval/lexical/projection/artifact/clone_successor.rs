use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use tracedecay_code_index::clones::CodeIndexCloneBodyV1;
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_parent_directory};
use tracedecay_private_fs::{create_private_file_retained, open_private_file};

use super::super::CodeLexicalProjectionMetadataV1;
use super::builder::{
    BuilderMutationGuardV1, compute_clone_section_digests, install_clone_freeze,
    register_builder_mutation_gate, sqlite_file_size, verify_clone_rows,
};
use super::format::{
    RECEIPT_RESERVATION_BYTES, VerifiedCodeLexicalArtifactV1, artifact_digest,
    decode_padded_receipt, metadata_digest, new_verified_receipt, padded_receipt,
};
use super::schema::{CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1, LexicalArtifactLayoutV1};
use super::{CodeLexicalArtifactErrorV1, checkpoint, open_builder_connection, sqlite_error};

pub struct CodeLexicalCloneSuccessorV1 {
    connection: Connection,
    mutation_gate: Arc<std::sync::atomic::AtomicU8>,
    staging_path: PathBuf,
    prior: VerifiedCodeLexicalArtifactV1,
    metadata: CodeLexicalProjectionMetadataV1,
}

impl CodeLexicalCloneSuccessorV1 {
    pub fn open_or_create(
        prior_path: impl AsRef<Path>,
        staging_path: impl AsRef<Path>,
        prior: VerifiedCodeLexicalArtifactV1,
        metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let staging_path = staging_path.as_ref();
        if staging_path.exists() {
            return Self::open(staging_path, prior, metadata, memory_budget_bytes);
        }
        initialize_successor(
            prior_path.as_ref(),
            staging_path,
            &prior,
            &metadata,
            memory_budget_bytes,
        )?;
        Self::open(staging_path, prior, metadata, memory_budget_bytes)
    }

    fn open(
        staging_path: &Path,
        prior: VerifiedCodeLexicalArtifactV1,
        metadata: CodeLexicalProjectionMetadataV1,
        memory_budget_bytes: usize,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let connection = open_builder_connection(staging_path, memory_budget_bytes)?;
        let mutation_gate = register_builder_mutation_gate(&connection)?;
        let (prior_digest, format_revision): (String, i64) = connection
            .query_row(
                "SELECT prior_artifact_digest, (SELECT format_revision FROM artifact_state WHERE singleton = 1) FROM clone_successor_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| {
                CodeLexicalArtifactErrorV1::Incompatible(format!(
                    "clone successor state is unavailable: {error}"
                ))
            })?;
        if prior_digest != prior.artifact_digest().as_str()
            || format_revision != i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1)
            || metadata_digest(&metadata)? != *prior.metadata_digest()
        {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "clone successor does not match its prior artifact or metadata".to_owned(),
            ));
        }
        Ok(Self {
            connection,
            mutation_gate,
            staging_path: staging_path.to_path_buf(),
            prior,
            metadata,
        })
    }

    pub fn next_cursor(
        &self,
    ) -> Result<Option<VerifiedSealedLexicalCursorV1>, CodeLexicalArtifactErrorV1> {
        let (next_page, bytes): (i64, Option<Vec<u8>>) = self
            .connection
            .query_row(
                "SELECT next_page_ordinal, next_cursor FROM clone_successor_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sqlite_error)?;
        let cursor = bytes
            .map(|bytes| {
                VerifiedSealedLexicalCursorV1::restore_persisted(&bytes)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
            })
            .transpose()?;
        if cursor.as_ref().map_or(next_page != 0, |cursor| {
            u64::try_from(next_page).ok() != Some(cursor.next_page_ordinal())
        }) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone successor page ordinal disagrees with its source cursor".to_owned(),
            ));
        }
        Ok(cursor)
    }

    pub fn append_page(
        &mut self,
        page: &VerifiedSealedLexicalPageV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        let _authority = BuilderMutationGuardV1::enter(&self.mutation_gate)?;
        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        let next_page: i64 = transaction
            .query_row(
                "SELECT next_page_ordinal FROM clone_successor_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        if u64::try_from(next_page).ok() != Some(page.page_ordinal()) {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "clone successor page is not the next source page".to_owned(),
            ));
        }
        verify_copied_source_page(&transaction, page)?;
        let next_cursor = page
            .next_cursor()
            .persisted_bytes()
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        append_clone_rows(&transaction, page, control)?;
        transaction
            .execute(
                "UPDATE clone_successor_state SET next_page_ordinal = ?1, next_cursor = ?2 WHERE singleton = 1",
                params![
                    i64::try_from(page.page_ordinal().saturating_add(1))
                        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                    next_cursor,
                ],
            )
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        transaction.commit().map_err(sqlite_error)
    }

    pub fn verify_resumed_page(
        &self,
        page: &VerifiedSealedLexicalPageV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        verify_copied_source_page(&self.connection, page)?;
        verify_clone_page_rows(&self.connection, page, control)
    }

    pub fn finish(
        &mut self,
        source: &VerifiedSealedLexicalSourceReceiptV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        verify_source_receipt(&self.prior, source)?;
        let next_page: i64 = self
            .connection
            .query_row(
                "SELECT next_page_ordinal FROM clone_successor_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        if u64::try_from(next_page).ok() != Some(source.page_count()) {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "clone successor has not consumed every source page".to_owned(),
            ));
        }
        let transaction = self.connection.transaction().map_err(sqlite_error)?;
        derive_clone_fingerprint_counts(&transaction)?;
        verify_clone_rows(&transaction, source)?;
        install_clone_freeze(&transaction, LexicalArtifactLayoutV1::V16)?;
        transaction
            .execute("DROP TABLE clone_successor_state", [])
            .map_err(sqlite_error)?;
        let mut sections = self
            .prior
            .section_digests()
            .iter()
            .take(11)
            .cloned()
            .collect::<Vec<_>>();
        if sections.len() != 11 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone successor prior is missing lexical section digests".to_owned(),
            ));
        }
        sections.extend(compute_clone_section_digests(
            &transaction,
            control,
            LexicalArtifactLayoutV1::V16,
        )?);
        let metadata_digest = metadata_digest(&self.metadata)?;
        let digest = artifact_digest(
            &metadata_digest,
            source.source_state_digest(),
            source.format_revision(),
            source.page_count(),
            source.total_chunks(),
            source.total_payload_bytes(),
            source.total_imports(),
            source.import_payload_bytes(),
            source.import_dictionary_digest(),
            source.cumulative_digest(),
            &sections,
            CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1,
        )?;
        let file_size = sqlite_file_size(&transaction)?;
        let receipt = new_verified_receipt(
            self.metadata.clone(),
            metadata_digest,
            source,
            digest,
            sections,
            file_size,
            LexicalArtifactLayoutV1::V16,
        );
        let encoded = padded_receipt(&receipt)?;
        transaction
            .execute(
                "UPDATE artifact_state SET format_revision = ?1, receipt = ?2 WHERE singleton = 1",
                params![i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1), encoded,],
            )
            .map_err(sqlite_error)?;
        checkpoint(control)?;
        transaction.commit().map_err(sqlite_error)?;
        open_private_file(&self.staging_path)
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?
            .sync_all()
            .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
        Ok(receipt)
    }
}

fn initialize_successor(
    prior_path: &Path,
    staging_path: &Path,
    prior: &VerifiedCodeLexicalArtifactV1,
    metadata: &CodeLexicalProjectionMetadataV1,
    memory_budget_bytes: usize,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut source = open_private_file(prior_path)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    let mut target = create_private_file_retained(staging_path)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.into_error().to_string()))?;
    io::copy(&mut source, &mut target)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    target
        .sync_all()
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))?;
    drop(target);
    let connection = open_builder_connection(staging_path, memory_budget_bytes)?;
    let stored: Vec<u8> = connection
        .query_row(
            "SELECT receipt FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if decode_padded_receipt(&stored)?.as_ref() != Some(prior)
        || metadata_digest(metadata)? != *prior.metadata_digest()
    {
        return Err(CodeLexicalArtifactErrorV1::Incompatible(
            "clone successor prior artifact does not match its receipt".to_owned(),
        ));
    }
    reset_clone_tables(&connection)?;
    connection
        .execute_batch(
            "CREATE TABLE clone_successor_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                prior_artifact_digest TEXT NOT NULL,
                next_page_ordinal INTEGER NOT NULL,
                next_cursor BLOB
             );",
        )
        .map_err(sqlite_error)?;
    connection
        .execute(
            "INSERT INTO clone_successor_state(singleton, prior_artifact_digest, next_page_ordinal, next_cursor) VALUES (1, ?1, 0, NULL)",
            [prior.artifact_digest().as_str()],
        )
        .map_err(sqlite_error)?;
    connection
        .execute(
            "UPDATE artifact_state SET format_revision = ?1, receipt = ?2 WHERE singleton = 1",
            params![
                i64::from(CODE_LEXICAL_ARTIFACT_FORMAT_REVISION_V1),
                vec![0u8; RECEIPT_RESERVATION_BYTES],
            ],
        )
        .map_err(sqlite_error)?;
    connection
        .execute("DELETE FROM finalization_state", [])
        .map_err(sqlite_error)?;
    connection
        .execute_batch("PRAGMA optimize;")
        .map_err(sqlite_error)?;
    drop(connection);
    sync_parent_directory(staging_path, DirectorySyncPolicy::Strict)
        .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))
}

const RESET_CLONE_TABLES_SQL: &str =
    "DROP TRIGGER IF EXISTS frozen_clone_body_payloads_insert;
             DROP TRIGGER IF EXISTS frozen_clone_occurrences_insert;
             DROP TRIGGER IF EXISTS frozen_clone_exact_postings_insert;
             DROP TRIGGER IF EXISTS frozen_clone_fingerprint_counts_insert;
             DROP TRIGGER IF EXISTS frozen_clone_fingerprint_postings_insert;
             DROP TRIGGER IF EXISTS builder_gate_clone_body_payloads_insert;
             DROP TRIGGER IF EXISTS builder_gate_clone_occurrences_insert;
             DROP TRIGGER IF EXISTS builder_gate_clone_exact_postings_insert;
             DROP TRIGGER IF EXISTS builder_gate_clone_fingerprint_postings_insert;
             DROP TRIGGER IF EXISTS immutable_clone_body_payloads_update;
             DROP TRIGGER IF EXISTS immutable_clone_body_payloads_delete;
             DROP TRIGGER IF EXISTS immutable_clone_occurrences_update;
             DROP TRIGGER IF EXISTS immutable_clone_occurrences_delete;
             DROP TRIGGER IF EXISTS immutable_clone_exact_postings_update;
             DROP TRIGGER IF EXISTS immutable_clone_exact_postings_delete;
             DROP TRIGGER IF EXISTS immutable_clone_fingerprint_postings_update;
             DROP TRIGGER IF EXISTS immutable_clone_fingerprint_postings_delete;
             DROP TRIGGER IF EXISTS immutable_clone_fingerprint_counts_update;
             DROP TRIGGER IF EXISTS immutable_clone_fingerprint_counts_delete;
             DROP TABLE IF EXISTS clone_fingerprint_counts;
             DROP TABLE IF EXISTS clone_fingerprint_postings;
             DROP TABLE IF EXISTS clone_exact_postings;
             DROP TABLE IF EXISTS clone_occurrences;
             DROP TABLE IF EXISTS clone_body_payloads;
             CREATE TABLE clone_body_payloads (
                payload_digest TEXT PRIMARY KEY,
                payload BLOB NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE clone_occurrences (
                symbol_occurrence_id TEXT PRIMARY KEY,
                payload_digest TEXT NOT NULL,
                path TEXT NOT NULL,
                body_start INTEGER NOT NULL,
                body_end INTEGER NOT NULL,
                occurrence BLOB NOT NULL
             ) WITHOUT ROWID;
             CREATE TABLE clone_exact_postings (
                class INTEGER NOT NULL,
                normalization_revision INTEGER NOT NULL,
                digest TEXT NOT NULL,
                symbol_occurrence_id TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                PRIMARY KEY(class, normalization_revision, digest, symbol_occurrence_id)
             ) WITHOUT ROWID;
             CREATE TABLE clone_fingerprint_counts (
                language TEXT NOT NULL,
                class INTEGER NOT NULL,
                normalization_revision INTEGER NOT NULL,
                fingerprint INTEGER NOT NULL,
                posting_count INTEGER NOT NULL,
                PRIMARY KEY(language, class, normalization_revision, fingerprint)
             ) WITHOUT ROWID;
             CREATE TABLE clone_fingerprint_postings (
                language TEXT NOT NULL,
                class INTEGER NOT NULL,
                normalization_revision INTEGER NOT NULL,
                fingerprint INTEGER NOT NULL,
                symbol_occurrence_id TEXT NOT NULL,
                token_position INTEGER NOT NULL,
                payload_digest TEXT NOT NULL,
                body_digest TEXT NOT NULL,
                PRIMARY KEY(language, class, normalization_revision, fingerprint, symbol_occurrence_id, token_position)
             ) WITHOUT ROWID;
             CREATE TRIGGER builder_gate_clone_body_payloads_insert BEFORE INSERT ON clone_body_payloads WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
             CREATE TRIGGER builder_gate_clone_occurrences_insert BEFORE INSERT ON clone_occurrences WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
             CREATE TRIGGER builder_gate_clone_exact_postings_insert BEFORE INSERT ON clone_exact_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
             CREATE TRIGGER builder_gate_clone_fingerprint_postings_insert BEFORE INSERT ON clone_fingerprint_postings WHEN tracedecay_lexical_builder_append_authorized() != 1 BEGIN SELECT RAISE(ABORT, 'private lexical builder mutation required'); END;
             CREATE TRIGGER immutable_clone_body_payloads_update BEFORE UPDATE ON clone_body_payloads BEGIN SELECT RAISE(ABORT, 'immutable clone body payloads'); END;
             CREATE TRIGGER immutable_clone_body_payloads_delete BEFORE DELETE ON clone_body_payloads BEGIN SELECT RAISE(ABORT, 'immutable clone body payloads'); END;
             CREATE TRIGGER immutable_clone_occurrences_update BEFORE UPDATE ON clone_occurrences BEGIN SELECT RAISE(ABORT, 'immutable clone occurrences'); END;
             CREATE TRIGGER immutable_clone_occurrences_delete BEFORE DELETE ON clone_occurrences BEGIN SELECT RAISE(ABORT, 'immutable clone occurrences'); END;
             CREATE TRIGGER immutable_clone_exact_postings_update BEFORE UPDATE ON clone_exact_postings BEGIN SELECT RAISE(ABORT, 'immutable clone exact postings'); END;
             CREATE TRIGGER immutable_clone_exact_postings_delete BEFORE DELETE ON clone_exact_postings BEGIN SELECT RAISE(ABORT, 'immutable clone exact postings'); END;
             CREATE TRIGGER immutable_clone_fingerprint_postings_update BEFORE UPDATE ON clone_fingerprint_postings BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint postings'); END;
             CREATE TRIGGER immutable_clone_fingerprint_postings_delete BEFORE DELETE ON clone_fingerprint_postings BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint postings'); END;
             CREATE TRIGGER immutable_clone_fingerprint_counts_update BEFORE UPDATE ON clone_fingerprint_counts BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint counts'); END;
             CREATE TRIGGER immutable_clone_fingerprint_counts_delete BEFORE DELETE ON clone_fingerprint_counts BEGIN SELECT RAISE(ABORT, 'immutable clone fingerprint counts'); END;";

fn reset_clone_tables(connection: &Connection) -> Result<(), CodeLexicalArtifactErrorV1> {
    connection
        .execute_batch(RESET_CLONE_TABLES_SQL)
        .map_err(sqlite_error)
}

fn append_clone_rows(
    transaction: &rusqlite::Transaction<'_>,
    page: &VerifiedSealedLexicalPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for body in page.clone_bodies() {
        checkpoint(control)?;
        let payload = serde_json::to_vec(&body.payload)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let occurrence = serde_json::to_vec(&body.occurrence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO clone_body_payloads(payload_digest, payload) VALUES (?1, ?2) ON CONFLICT(payload_digest) DO NOTHING",
                params![body.payload.payload_digest.as_str(), payload],
            )
            .map_err(sqlite_error)?;
        let stored: Vec<u8> = transaction
            .query_row(
                "SELECT payload FROM clone_body_payloads WHERE payload_digest = ?1",
                [body.payload.payload_digest.as_str()],
                |row| row.get(0),
            )
            .map_err(sqlite_error)?;
        if stored
            != serde_json::to_vec(&body.payload)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "clone successor payload digest collision".to_owned(),
            ));
        }
        transaction
            .execute(
                "INSERT INTO clone_occurrences(symbol_occurrence_id, payload_digest, path, body_start, body_end, occurrence) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    body.occurrence.symbol_occurrence_id.as_str(),
                    body.occurrence.payload_digest.as_str(),
                    body.occurrence.path,
                    i64::try_from(body.occurrence.body_span.start_byte)
                        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                    i64::try_from(body.occurrence.body_span.end_byte)
                        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                    occurrence,
                ],
            )
            .map_err(sqlite_error)?;
        for key in body.payload.exact_keys(body.occurrence.eligibility) {
            transaction
                .execute(
                    "INSERT INTO clone_exact_postings(class, normalization_revision, digest, symbol_occurrence_id, payload_digest) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        i64::from(key.class as u8),
                        i64::from(key.normalization_revision),
                        key.digest.as_str(),
                        body.occurrence.symbol_occurrence_id.as_str(),
                        body.occurrence.payload_digest.as_str(),
                    ],
                )
                .map_err(sqlite_error)?;
        }
        append_clone_fingerprints(transaction, body)?;
    }
    Ok(())
}

fn append_clone_fingerprints(
    transaction: &rusqlite::Transaction<'_>,
    body: &CodeIndexCloneBodyV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let Some(stream) = body.payload.fingerprint_stream(body.occurrence.eligibility) else {
        return Ok(());
    };
    for position in body
        .payload
        .fingerprint_positions(body.occurrence.eligibility)
        .map_err(CodeLexicalArtifactErrorV1::Contract)?
    {
        transaction
            .execute(
                "INSERT INTO clone_fingerprint_postings(language, class, normalization_revision, fingerprint, symbol_occurrence_id, token_position, payload_digest, body_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    body.payload.language,
                    i64::from(stream.class as u8),
                    i64::from(stream.normalization_revision),
                    i64::try_from(position.fingerprint)
                        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                    body.occurrence.symbol_occurrence_id.as_str(),
                    i64::from(position.token_position),
                    body.occurrence.payload_digest.as_str(),
                    body.payload.body_digest.as_str(),
                ],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn verify_copied_source_page(
    connection: &Connection,
    page: &VerifiedSealedLexicalPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let stored: Option<(String, String, Vec<u8>)> = connection
        .query_row(
            "SELECT page_digest, cumulative_digest, next_cursor FROM source_pages WHERE page_ordinal = ?1",
            [i64::try_from(page.page_ordinal())
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let next_cursor = page
        .next_cursor()
        .persisted_bytes()
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
    if stored
        != Some((
            page.page_digest().as_str().to_owned(),
            page.cumulative_digest().as_str().to_owned(),
            next_cursor,
        ))
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone successor page does not match the copied lexical source receipt".to_owned(),
        ));
    }
    Ok(())
}

fn verify_clone_page_rows(
    connection: &Connection,
    page: &VerifiedSealedLexicalPageV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for body in page.clone_bodies() {
        checkpoint(control)?;
        let expected_payload = serde_json::to_vec(&body.payload)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let stored_payload: Option<Vec<u8>> = connection
            .query_row(
                "SELECT payload FROM clone_body_payloads WHERE payload_digest = ?1",
                [body.payload.payload_digest.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if stored_payload.as_deref() != Some(expected_payload.as_slice()) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "resumed clone payload differs from its sealed source page".to_owned(),
            ));
        }

        let expected_occurrence = serde_json::to_vec(&body.occurrence)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let stored_occurrence: Option<(String, String, i64, i64, Vec<u8>)> = connection
            .query_row(
                "SELECT payload_digest, path, body_start, body_end, occurrence FROM clone_occurrences WHERE symbol_occurrence_id = ?1",
                [body.occurrence.symbol_occurrence_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let expected_span = (
            i64::try_from(body.occurrence.body_span.start_byte)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
            i64::try_from(body.occurrence.body_span.end_byte)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
        );
        if stored_occurrence.as_ref().map(|stored| {
            (
                stored.0.as_str(),
                stored.1.as_str(),
                stored.2,
                stored.3,
                stored.4.as_slice(),
            )
        }) != Some((
            body.occurrence.payload_digest.as_str(),
            body.occurrence.path.as_str(),
            expected_span.0,
            expected_span.1,
            expected_occurrence.as_slice(),
        )) {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "resumed clone occurrence differs from its sealed source page".to_owned(),
            ));
        }

        let expected_postings = body
            .payload
            .exact_keys(body.occurrence.eligibility)
            .into_iter()
            .map(|key| {
                (
                    i64::from(key.class as u8),
                    i64::from(key.normalization_revision),
                    key.digest.as_str().to_owned(),
                    body.occurrence.payload_digest.as_str().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let mut statement = connection
            .prepare(
                "SELECT class, normalization_revision, digest, payload_digest FROM clone_exact_postings WHERE symbol_occurrence_id = ?1 ORDER BY class, normalization_revision, digest",
            )
            .map_err(sqlite_error)?;
        let stored_postings = statement
            .query_map([body.occurrence.symbol_occurrence_id.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<(i64, i64, String, String)>, _>>()
            .map_err(sqlite_error)?;
        if stored_postings != expected_postings {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "resumed clone postings differ from their sealed source page".to_owned(),
            ));
        }
        verify_clone_fingerprint_page_rows(connection, body)?;
    }
    Ok(())
}

type CloneFingerprintRowV1 = (String, i64, i64, i64, i64, String, String);

fn verify_clone_fingerprint_page_rows(
    connection: &Connection,
    body: &CodeIndexCloneBodyV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut expected = Vec::new();
    if let Some(stream) = body.payload.fingerprint_stream(body.occurrence.eligibility) {
        for position in body
            .payload
            .fingerprint_positions(body.occurrence.eligibility)
            .map_err(CodeLexicalArtifactErrorV1::Contract)?
        {
            expected.push((
                body.payload.language.clone(),
                i64::from(stream.class as u8),
                i64::from(stream.normalization_revision),
                i64::try_from(position.fingerprint)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                i64::from(position.token_position),
                body.occurrence.payload_digest.as_str().to_owned(),
                body.payload.body_digest.as_str().to_owned(),
            ));
        }
    }
    expected.sort();
    let mut statement = connection
        .prepare(
            "SELECT language, class, normalization_revision, fingerprint, token_position, payload_digest, body_digest FROM clone_fingerprint_postings WHERE symbol_occurrence_id = ?1 ORDER BY language, class, normalization_revision, fingerprint, token_position",
        )
        .map_err(sqlite_error)?;
    let stored = statement
        .query_map([body.occurrence.symbol_occurrence_id.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<CloneFingerprintRowV1>, _>>()
        .map_err(sqlite_error)?;
    if stored != expected {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "resumed clone fingerprints differ from their sealed source page".to_owned(),
        ));
    }
    Ok(())
}

fn verify_source_receipt(
    prior: &VerifiedCodeLexicalArtifactV1,
    source: &VerifiedSealedLexicalSourceReceiptV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if prior.source_state_digest() != source.source_state_digest()
        || prior.source_cumulative_digest() != source.cumulative_digest()
        || prior.page_count() != source.page_count()
        || prior.total_chunks() != source.total_chunks()
        || prior.total_payload_bytes() != source.total_payload_bytes()
        || prior.total_imports() != source.total_imports()
        || prior.import_payload_bytes() != source.import_payload_bytes()
        || prior.import_dictionary_digest() != source.import_dictionary_digest()
    {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "clone successor source receipt differs from its lexical predecessor".to_owned(),
        ));
    }
    Ok(())
}

fn derive_clone_fingerprint_counts(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(
            "INSERT INTO clone_fingerprint_counts(language, class, normalization_revision, fingerprint, posting_count)
             SELECT language, class, normalization_revision, fingerprint, COUNT(*)
             FROM clone_fingerprint_postings
             GROUP BY language, class, normalization_revision, fingerprint;",
        )
        .map_err(sqlite_error)
}
