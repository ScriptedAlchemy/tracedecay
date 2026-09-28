//! Carry a published parent text artifact into a successor staging file.
//!
//! A successor generation reuses unchanged file occurrence ids. Those rows,
//! postings, and clone occurrences stay in the copied artifact. Only files
//! whose occurrence id is new are appended, and only occurrences the child
//! dropped are removed. A restart therefore rebuilds changed and unfinished
//! files instead of replaying the sealed corpus.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use roaring::RoaringBitmap;
use rusqlite::{Connection, OptionalExtension, Params, Transaction, params};
use serde::Deserialize;
use tracedecay_domain::{CodeGenerationId, FileOccurrenceId};
use tracedecay_private_fs::create_private_file_retained;

use super::builder::{
    BuilderMutationGuardV1, append_staged_run, drop_seal_triggers, ensure_carried_builder_triggers,
    register_builder_mutation_gate, staging_sibling,
};
use super::format::{
    PostingListDecoderV1, PostingListEncoderV1, RECEIPT_RESERVATION_BYTES, content_metadata_bytes,
    decode_document_set, decode_fingerprint_postings, decode_term_lists,
    encode_fingerprint_postings, encode_term_lists, metadata_digest, term_lists_bytes,
};
use super::prepared::document_ngram_keys;
use super::row_codec::{
    BlockRowV1, ConnectionRowDictionaryV1, RowDictionaryEntryV1, RowDictionaryV1,
    decode_artifact_row, decode_row_block, encode_row_blocks, row_file_reference,
    scoring_preface_rows,
};
use super::schema::field_code;
use super::{checkpoint, sqlite_error};
use crate::retrieval::lexical::LexicalFieldV1;
use crate::retrieval::lexical::projection::CodeLexicalProjectionMetadataV1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::CodeLexicalArtifactErrorV1;

const CARRIED_SHIFT_TABLE: &str = "carried_document_shift";
const CARRIED_REBUILD_TABLE: &str = "carried_rebuild_occurrence";
const CARRIED_RETIRED_NGRAM_TABLE: &str = "carried_retired_ngram";
/// Staging-name suffix of a carry still being copied and planned. Retention
/// and the staging sweep treat it as a sidecar of its staging database.
const CARRYING_SUFFIX: &str = "-carrying";
/// Past this multiple of changed documents, rewriting every n-gram list is
/// smaller than patching the keys those documents touch.
const CARRIED_NGRAM_PATCH_DOCUMENT_FACTOR: i64 = 4;

#[derive(Deserialize)]
struct StoredContentPathsV1 {
    logical_paths: BTreeMap<FileOccurrenceId, String>,
}

pub(super) fn carried_delta_pending(
    connection: &Connection,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    table_exists(connection, CARRIED_SHIFT_TABLE)
}

pub(super) fn read_carried_document_shift(
    connection: &Connection,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    if !table_exists(connection, CARRIED_SHIFT_TABLE)? {
        return Ok(0);
    }
    let shift: i64 = connection
        .query_row(
            "SELECT shift FROM carried_document_shift WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    u64::try_from(shift).map_err(|_| {
        CodeLexicalArtifactErrorV1::Corrupt(
            "carried lexical artifact document shift is negative".to_owned(),
        )
    })
}

pub(super) fn read_carried_rebuild_occurrences(
    connection: &Connection,
) -> Result<Option<BTreeSet<FileOccurrenceId>>, CodeLexicalArtifactErrorV1> {
    if !table_exists(connection, CARRIED_SHIFT_TABLE)? {
        return Ok(None);
    }
    if !table_exists(connection, CARRIED_REBUILD_TABLE)? {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "carried lexical artifact is missing its rebuild occurrence roster".to_owned(),
        ));
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT occurrence FROM {CARRIED_REBUILD_TABLE} ORDER BY occurrence"
        ))
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut occurrences = BTreeSet::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let occurrence: String = row.get(0).map_err(sqlite_error)?;
        let occurrence = FileOccurrenceId::new(occurrence).map_err(|error| {
            CodeLexicalArtifactErrorV1::Corrupt(format!(
                "carried lexical rebuild occurrence is invalid: {error}"
            ))
        })?;
        occurrences.insert(occurrence);
    }
    Ok(Some(occurrences))
}

/// Copy `parent` onto `staging` and retire occurrences the child metadata no
/// longer names. Returns false when no unchanged occurrence can be carried;
/// the staging path is absent in that case.
///
/// The copy and its carry plan are written beside `staging` and renamed onto
/// it only after the plan commits. A process killed mid-copy leaves a torn
/// file under that sibling name, which the next carry or staging sweep
/// removes, never a torn staging database a restart would try to resume.
pub(super) fn stage_carried_parent(
    parent: &Path,
    staging: &Path,
    metadata: &CodeLexicalProjectionMetadataV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    checkpoint(control)?;
    let carrying = staging_sibling(staging, CARRYING_SUFFIX)?;
    remove_sqlite_family(&carrying)?;
    copy_private_file(parent, &carrying)?;
    let mut connection = Connection::open(&carrying).map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(sqlite_error)?;
    let carried = (|| {
        let parent_paths = read_stored_paths(&connection)?;
        let unchanged = metadata
            .logical_paths
            .keys()
            .filter(|occurrence| parent_paths.contains_key(*occurrence))
            .count();
        if unchanged == 0 {
            return Ok(false);
        }
        let retired: BTreeSet<FileOccurrenceId> = parent_paths
            .keys()
            .filter(|occurrence| !metadata.logical_paths.contains_key(*occurrence))
            .cloned()
            .collect();
        let rebuild: BTreeSet<FileOccurrenceId> = metadata
            .logical_paths
            .keys()
            .filter(|occurrence| !parent_paths.contains_key(*occurrence))
            .cloned()
            .collect();
        let gate = register_builder_mutation_gate(&connection)?;
        let transaction = connection.transaction().map_err(sqlite_error)?;
        let _guard = BuilderMutationGuardV1::enter(&gate)?;
        drop_seal_triggers(&transaction)?;
        let shift = retire_occurrences(&transaction, &metadata.generation, &retired, control)?;
        clear_source_receipts(&transaction)?;
        ensure_staging_tables(&transaction)?;
        ensure_carried_builder_triggers(&transaction)?;
        write_carried_plan(&transaction, metadata, shift, &rebuild)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(true)
    })();
    drop(connection);
    match carried {
        Ok(true) => {
            std::fs::rename(&carrying, staging).map_err(|error| {
                CodeLexicalArtifactErrorV1::Io(format!(
                    "install carried lexical staging {}: {error}",
                    staging.display()
                ))
            })?;
            Ok(true)
        }
        Ok(false) => {
            remove_sqlite_family(&carrying)?;
            Ok(false)
        }
        Err(error) => {
            remove_sqlite_family(&carrying)?;
            Err(error)
        }
    }
}

/// Fold staged pages of a carried artifact into the sealed tables copied
/// from its parent. The caller then patches or rebuilds n-gram lists, which
/// are derived from row blocks rather than runs.
pub(super) fn merge_carried_staging(
    transaction: &Transaction<'_>,
    gate: &std::sync::Arc<std::sync::atomic::AtomicU8>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let _guard = BuilderMutationGuardV1::enter(gate)?;
    checkpoint(control)?;
    // Staging reinstalls seal triggers so append cannot rewrite the parent
    // tables. Folding the new runs has to drop them again.
    drop_seal_triggers(transaction)?;
    merge_term_runs(transaction, control)?;
    merge_exact_runs(transaction, control)?;
    merge_row_chunks(transaction)?;
    merge_row_dictionary(transaction)?;
    merge_field_stats(transaction)?;
    merge_clone_fingerprints(transaction, control)?;
    transaction
        .execute_batch(
            "DROP TABLE IF EXISTS term_posting_runs;
             DROP TABLE IF EXISTS exact_posting_runs;
             DROP TABLE IF EXISTS row_chunk_pages;
             DROP TABLE IF EXISTS row_dictionary_pages;
             DROP TABLE IF EXISTS field_stats_staging;
             DROP TABLE IF EXISTS clone_fingerprint_postings_pages;",
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn copy_private_file(parent: &Path, staging: &Path) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut source = std::fs::File::open(parent).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!(
            "open parent lexical artifact {}: {error}",
            parent.display()
        ))
    })?;
    let mut destination = create_private_file_retained(staging)
        .map_err(|failure| CodeLexicalArtifactErrorV1::Io(failure.to_string()))?;
    std::io::copy(&mut source, &mut destination).map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!("copy parent lexical artifact: {error}"))
    })?;
    destination.sync_all().map_err(|error| {
        CodeLexicalArtifactErrorV1::Io(format!("sync carried lexical artifact: {error}"))
    })?;
    Ok(())
}

fn remove_sqlite_family(path: &Path) -> Result<(), CodeLexicalArtifactErrorV1> {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let candidate = staging_sibling(path, suffix)?;
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CodeLexicalArtifactErrorV1::Io(format!(
                    "remove carried lexical staging {}: {error}",
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

fn read_stored_paths(
    connection: &Connection,
) -> Result<BTreeMap<FileOccurrenceId, String>, CodeLexicalArtifactErrorV1> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT metadata FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    let stored: StoredContentPathsV1 = serde_json::from_slice(&bytes).map_err(|error| {
        CodeLexicalArtifactErrorV1::Corrupt(format!(
            "parent lexical artifact metadata is unreadable: {error}"
        ))
    })?;
    Ok(stored.logical_paths)
}

struct CarriedKeptRowV1 {
    document_id: u32,
    chunk_id: String,
    row: Vec<u8>,
    text: String,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
    trimmed_normalized_len: usize,
}

fn retire_occurrences(
    transaction: &Transaction<'_>,
    generation: &CodeGenerationId,
    retired: &BTreeSet<FileOccurrenceId>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(&format!(
            "CREATE TABLE {CARRIED_RETIRED_NGRAM_TABLE}(
                kind INTEGER NOT NULL,
                ngram INTEGER NOT NULL,
                document_id INTEGER NOT NULL,
                PRIMARY KEY(kind, ngram, document_id)
            ) WITHOUT ROWID;"
        ))
        .map_err(sqlite_error)?;
    let mut retired_documents = HashSet::new();
    let mut retired_paths = BTreeSet::new();
    let mut field_deltas = BTreeMap::<i64, i64>::new();
    let mut retired_ngrams = Vec::new();
    if !retired.is_empty() {
        let mut statement = transaction
            .prepare("SELECT first_document, payload FROM row_blocks ORDER BY first_document")
            .map_err(sqlite_error)?;
        let blocks = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        drop(statement);
        let dictionary = ConnectionRowDictionaryV1::new(transaction);
        for (first_document, payload) in blocks {
            checkpoint(control)?;
            let preface = scoring_preface_rows(first_document, &payload)?;
            let rows = decode_row_block(first_document, &payload)?;
            if preface.len() != rows.len() {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "lexical artifact scoring preface does not match its row block".to_owned(),
                ));
            }
            let mut kept = Vec::new();
            let mut retired_in_block = false;
            for (preface_row, row) in preface.into_iter().zip(rows) {
                if preface_row.document_id != row.document_id {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact scoring preface document does not match its row"
                            .to_owned(),
                    ));
                }
                let entry = dictionary.entry(row_file_reference(&row.row)?)?;
                let RowDictionaryEntryV1::File {
                    file_occurrence_id,
                    logical_path,
                    ..
                } = entry.as_ref()
                else {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "lexical artifact row file reference resolved to a non-file entry"
                            .to_owned(),
                    ));
                };
                let occurrence =
                    FileOccurrenceId::new(file_occurrence_id.as_str()).map_err(|error| {
                        CodeLexicalArtifactErrorV1::Corrupt(format!(
                            "carried lexical row file occurrence is invalid: {error}"
                        ))
                    })?;
                if retired.contains(&occurrence) {
                    retired_in_block = true;
                    retired_documents.insert(row.document_id);
                    retired_paths.insert(logical_path.clone());
                    let decoded = decode_artifact_row(
                        generation,
                        &row.chunk_id,
                        &row.row,
                        &row.text,
                        &dictionary,
                    )?;
                    retired_ngrams.extend(
                        document_ngram_keys(
                            &super::super::normalized_search_text(&decoded),
                            decoded.sanitized_text.as_str(),
                            decoded.normalized_text.as_str(),
                            control,
                        )?
                        .into_iter()
                        .map(|(kind, ngram)| (kind, ngram, row.document_id)),
                    );
                    for (field, length) in preface_row.field_lengths {
                        let length = i64::try_from(length).map_err(|_| {
                            CodeLexicalArtifactErrorV1::Contract(
                                "carried lexical field length exceeds i64".to_owned(),
                            )
                        })?;
                        let code = field_code(field);
                        let delta = field_deltas.entry(code).or_insert(0);
                        *delta = delta.checked_sub(length).ok_or_else(|| {
                            CodeLexicalArtifactErrorV1::Contract(
                                "carried lexical field-stat delta overflowed".to_owned(),
                            )
                        })?;
                    }
                } else {
                    kept.push(CarriedKeptRowV1 {
                        document_id: row.document_id,
                        chunk_id: row.chunk_id,
                        row: row.row,
                        text: row.text,
                        field_lengths: preface_row.field_lengths,
                        trimmed_normalized_len: preface_row.trimmed_normalized_len,
                    });
                }
            }
            if !retired_in_block {
                continue;
            }
            transaction
                .execute(
                    "DELETE FROM row_blocks WHERE first_document = ?1",
                    [first_document],
                )
                .map_err(sqlite_error)?;
            if kept.is_empty() {
                continue;
            }
            let encoded = encode_row_blocks(
                &kept
                    .iter()
                    .map(|row| BlockRowV1 {
                        document_id: i64::from(row.document_id),
                        chunk_id: row.chunk_id.as_str(),
                        parent_chunk_id: None,
                        row: row.row.as_slice(),
                        text: row.text.as_str(),
                        field_lengths: &row.field_lengths,
                        trimmed_normalized_len: row.trimmed_normalized_len,
                    })
                    .collect::<Vec<_>>(),
            )?;
            let mut insert = transaction
                .prepare("INSERT INTO row_blocks(first_document, payload) VALUES (?1, ?2)")
                .map_err(sqlite_error)?;
            for (first_document, payload) in encoded {
                insert
                    .execute(params![first_document, payload])
                    .map_err(sqlite_error)?;
            }
        }
        // The dictionary borrow ends before the next statements.
        drop(dictionary);
        if !retired_ngrams.is_empty() {
            let mut insert = transaction
                .prepare(&format!(
                    "INSERT OR IGNORE INTO {CARRIED_RETIRED_NGRAM_TABLE}(kind, ngram, document_id) VALUES (?1, ?2, ?3)"
                ))
                .map_err(sqlite_error)?;
            for (kind, ngram, document) in retired_ngrams {
                insert
                    .execute(params![kind, ngram, i64::from(document)])
                    .map_err(sqlite_error)?;
            }
        }
        scrub_term_postings(transaction, &retired_documents, control)?;
        scrub_exact_postings(transaction, &retired_documents, control)?;
        scrub_fingerprints(transaction, &retired_paths, control)?;
        delete_retired_clones(transaction, &retired_paths)?;
        apply_field_deltas(transaction, &field_deltas)?;
        let mut delete_chunks = transaction
            .prepare("DELETE FROM row_chunks WHERE document_id = ?1")
            .map_err(sqlite_error)?;
        for document in &retired_documents {
            delete_chunks
                .execute([i64::from(*document)])
                .map_err(sqlite_error)?;
        }
    }
    let max_document: Option<i64> = transaction
        .query_row("SELECT MAX(document_id) FROM row_chunks", [], |row| {
            row.get(0)
        })
        .map_err(sqlite_error)?;
    let shift = match max_document {
        None => 0,
        Some(max_document) => u64::try_from(max_document)
            .map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical document id is negative".to_owned(),
                )
            })?
            .checked_add(1)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Contract(
                    "carried lexical document shift overflowed".to_owned(),
                )
            })?,
    };
    Ok(shift)
}

fn clear_source_receipts(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    // A sealed parent keeps `source_pages` and drops the per-page cursor
    // table at the statistics step. Delete only the tables that are present;
    // the cursor table is recreated empty before append.
    let mut statements = vec![
        "DELETE FROM source_pages".to_owned(),
        "UPDATE content_epoch SET epoch = 0 WHERE singleton = 1".to_owned(),
    ];
    if table_exists(transaction, "source_page_cursors")? {
        statements.insert(1, "DELETE FROM source_page_cursors".to_owned());
    }
    transaction
        .execute_batch(&statements.join(";\n"))
        .map_err(sqlite_error)
}

fn ensure_staging_tables(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS finalization_state (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                state BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS source_page_cursors (
                page_ordinal INTEGER PRIMARY KEY,
                page_digest TEXT NOT NULL,
                cumulative_digest TEXT NOT NULL,
                payload_bytes INTEGER NOT NULL,
                next_cursor BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS row_chunk_pages (
                document_id INTEGER PRIMARY KEY,
                chunk_id BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS term_posting_runs (
                page_ordinal INTEGER NOT NULL,
                term TEXT NOT NULL,
                field INTEGER NOT NULL,
                postings BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, term, field)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS exact_posting_runs (
                page_ordinal INTEGER NOT NULL,
                term_id INTEGER NOT NULL,
                field INTEGER NOT NULL,
                documents BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, term_id, field)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS row_dictionary_pages (
                page_ordinal INTEGER NOT NULL,
                entry_id INTEGER NOT NULL,
                entry BLOB NOT NULL,
                PRIMARY KEY(page_ordinal, entry_id)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS field_stats_staging (
                field INTEGER PRIMARY KEY,
                total_length INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS clone_fingerprint_postings_pages (
                language TEXT NOT NULL,
                class INTEGER NOT NULL,
                normalization_revision INTEGER NOT NULL,
                fingerprint INTEGER NOT NULL,
                occurrence_ordinal INTEGER NOT NULL,
                token_position INTEGER NOT NULL
            );",
        )
        .map_err(sqlite_error)
}

fn write_carried_plan(
    transaction: &Transaction<'_>,
    metadata: &CodeLexicalProjectionMetadataV1,
    shift: u64,
    rebuild: &BTreeSet<FileOccurrenceId>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let metadata_bytes = content_metadata_bytes(metadata)?;
    let digest = metadata_digest(metadata)?;
    let shift = i64::try_from(shift).map_err(|_| {
        CodeLexicalArtifactErrorV1::Contract(
            "carried lexical document shift exceeds i64".to_owned(),
        )
    })?;
    transaction
        .execute(
            "UPDATE artifact_state SET metadata = ?1, metadata_digest = ?2, receipt = ?3 WHERE singleton = 1",
            params![
                metadata_bytes,
                digest.as_str(),
                vec![0u8; RECEIPT_RESERVATION_BYTES]
            ],
        )
        .map_err(sqlite_error)?;
    transaction
        .execute_batch(&format!(
            "DROP TABLE IF EXISTS {CARRIED_SHIFT_TABLE};
             DROP TABLE IF EXISTS {CARRIED_REBUILD_TABLE};
             CREATE TABLE {CARRIED_SHIFT_TABLE}(singleton INTEGER PRIMARY KEY CHECK(singleton = 1), shift INTEGER NOT NULL);
             CREATE TABLE {CARRIED_REBUILD_TABLE}(occurrence TEXT PRIMARY KEY) WITHOUT ROWID;"
        ))
        .map_err(sqlite_error)?;
    transaction
        .execute(
            &format!("INSERT INTO {CARRIED_SHIFT_TABLE}(singleton, shift) VALUES (1, ?1)"),
            [shift],
        )
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare(&format!(
            "INSERT INTO {CARRIED_REBUILD_TABLE}(occurrence) VALUES (?1)"
        ))
        .map_err(sqlite_error)?;
    for occurrence in rebuild {
        insert
            .execute([occurrence.as_str()])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn scrub_term_postings(
    transaction: &Transaction<'_>,
    retired: &HashSet<u32>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if retired.is_empty() {
        return Ok(());
    }
    let subtoken = field_code(LexicalFieldV1::Subtoken);
    let mut statement = transaction
        .prepare("SELECT term, lists FROM term_postings")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(statement);
    let mut update = transaction
        .prepare("UPDATE term_postings SET in_fuzzy = ?2, lists = ?3 WHERE term = ?1")
        .map_err(sqlite_error)?;
    let mut delete = transaction
        .prepare("DELETE FROM term_postings WHERE term = ?1")
        .map_err(sqlite_error)?;
    for (ordinal, (term, lists)) in rows.into_iter().enumerate() {
        if ordinal.is_multiple_of(1024) {
            checkpoint(control)?;
        }
        let inflated = term_lists_bytes(&lists)?;
        let decoded = decode_term_lists(&inflated)?;
        let mut changed = false;
        let mut kept = Vec::new();
        for (field, _frequency, postings) in decoded {
            match filter_posting_list(postings, true, retired)? {
                None => kept.push((field, postings.to_vec())),
                Some(filtered) if filtered.is_empty() => changed = true,
                Some(filtered) => {
                    changed = true;
                    kept.push((field, filtered));
                }
            }
        }
        if !changed {
            continue;
        }
        if kept.is_empty() {
            delete.execute([&term]).map_err(sqlite_error)?;
            continue;
        }
        let encoded = kept
            .iter()
            .map(|(field, postings)| {
                let count = u64::try_from(posting_count(postings, true)?).map_err(|_| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "carried lexical posting count exceeds u64".to_owned(),
                    )
                })?;
                Ok((*field, count, postings.clone()))
            })
            .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()?;
        let in_fuzzy = encoded.iter().any(|(field, _, _)| *field != subtoken);
        update
            .execute(params![term, in_fuzzy, encode_term_lists(&encoded)?])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn scrub_exact_postings(
    transaction: &Transaction<'_>,
    retired: &HashSet<u32>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if retired.is_empty() {
        return Ok(());
    }
    let mut statement = transaction
        .prepare("SELECT term_id, field, documents FROM exact_postings")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(statement);
    let mut update = transaction
        .prepare("UPDATE exact_postings SET documents = ?3 WHERE term_id = ?1 AND field = ?2")
        .map_err(sqlite_error)?;
    let mut delete = transaction
        .prepare("DELETE FROM exact_postings WHERE term_id = ?1 AND field = ?2")
        .map_err(sqlite_error)?;
    for (ordinal, (term_id, field, documents)) in rows.into_iter().enumerate() {
        if ordinal.is_multiple_of(1024) {
            checkpoint(control)?;
        }
        let Some(filtered) = filter_posting_list(&documents, false, retired)? else {
            continue;
        };
        if filtered.is_empty() {
            delete
                .execute(params![term_id, field])
                .map_err(sqlite_error)?;
        } else {
            update
                .execute(params![term_id, field, filtered])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

fn filter_posting_list(
    postings: &[u8],
    frequencies: bool,
    retired: &HashSet<u32>,
) -> Result<Option<Vec<u8>>, CodeLexicalArtifactErrorV1> {
    let mut encoder = PostingListEncoderV1::new(frequencies);
    let mut changed = false;
    for posting in PostingListDecoderV1::new(postings, frequencies) {
        let (document, frequency) = posting?;
        if retired.contains(&document) {
            changed = true;
            continue;
        }
        encoder.push(document, frequency).map_err(|_| {
            CodeLexicalArtifactErrorV1::Corrupt(
                "carried lexical posting list could not be rewritten".to_owned(),
            )
        })?;
    }
    if !changed {
        return Ok(None);
    }
    if encoder.len() == 0 {
        return Ok(Some(Vec::new()));
    }
    Ok(Some(encoder.finish()?))
}

fn posting_count(postings: &[u8], frequencies: bool) -> Result<usize, CodeLexicalArtifactErrorV1> {
    let mut count = 0usize;
    for posting in PostingListDecoderV1::new(postings, frequencies) {
        let _ = posting?;
        count = count.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "carried lexical posting count overflowed".to_owned(),
            )
        })?;
    }
    Ok(count)
}

fn scrub_fingerprints(
    transaction: &Transaction<'_>,
    retired_paths: &BTreeSet<String>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if retired_paths.is_empty() || !table_exists(transaction, "clone_fingerprint_postings")? {
        return Ok(());
    }
    let mut ordinals = transaction
        .prepare("SELECT ordinal FROM clone_occurrences WHERE path = ?1")
        .map_err(sqlite_error)?;
    let mut retired_ordinals = HashSet::new();
    for path in retired_paths {
        let rows = ordinals
            .query_map([path.as_str()], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)?;
        for ordinal in rows {
            let ordinal = ordinal.map_err(sqlite_error)?;
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "carried clone occurrence ordinal exceeds u32".to_owned(),
                )
            })?;
            retired_ordinals.insert(ordinal);
        }
    }
    drop(ordinals);
    if retired_ordinals.is_empty() {
        return Ok(());
    }
    let mut statement = transaction
        .prepare(
            "SELECT language, class, normalization_revision, fingerprint, postings FROM clone_fingerprint_postings",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(statement);
    let mut update = transaction
        .prepare(
            "UPDATE clone_fingerprint_postings SET posting_count = ?5, postings = ?6 WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
        )
        .map_err(sqlite_error)?;
    let mut delete = transaction
        .prepare(
            "DELETE FROM clone_fingerprint_postings WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
        )
        .map_err(sqlite_error)?;
    for (ordinal, (language, class, revision, fingerprint, postings)) in
        rows.into_iter().enumerate()
    {
        if ordinal.is_multiple_of(1024) {
            checkpoint(control)?;
        }
        let decoded = decode_fingerprint_postings(&postings)?;
        if !decoded
            .iter()
            .any(|(occurrence, _)| retired_ordinals.contains(occurrence))
        {
            continue;
        }
        let kept = decoded
            .into_iter()
            .filter(|(occurrence, _)| !retired_ordinals.contains(occurrence))
            .collect::<Vec<_>>();
        if kept.is_empty() {
            delete
                .execute(params![language, class, revision, fingerprint])
                .map_err(sqlite_error)?;
            continue;
        }
        let count = i64::try_from(kept.len()).map_err(|_| {
            CodeLexicalArtifactErrorV1::Contract(
                "carried clone fingerprint posting count exceeds i64".to_owned(),
            )
        })?;
        update
            .execute(params![
                language,
                class,
                revision,
                fingerprint,
                count,
                encode_fingerprint_postings(&kept)?
            ])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn delete_retired_clones(
    transaction: &Transaction<'_>,
    retired_paths: &BTreeSet<String>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if retired_paths.is_empty() {
        return Ok(());
    }
    let mut delete_postings = transaction
        .prepare(
            "DELETE FROM clone_exact_postings WHERE occurrence_ordinal IN (SELECT ordinal FROM clone_occurrences WHERE path = ?1)",
        )
        .map_err(sqlite_error)?;
    let mut delete_occurrences = transaction
        .prepare("DELETE FROM clone_occurrences WHERE path = ?1")
        .map_err(sqlite_error)?;
    for path in retired_paths {
        delete_postings
            .execute([path.as_str()])
            .map_err(sqlite_error)?;
        delete_occurrences
            .execute([path.as_str()])
            .map_err(sqlite_error)?;
    }
    transaction
        .execute(
            "DELETE FROM clone_body_payloads WHERE ordinal NOT IN (SELECT payload_ordinal FROM clone_occurrences)",
            [],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn apply_field_deltas(
    transaction: &Transaction<'_>,
    deltas: &BTreeMap<i64, i64>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut select = transaction
        .prepare("SELECT total_length FROM field_stats WHERE field = ?1")
        .map_err(sqlite_error)?;
    let mut update = transaction
        .prepare("UPDATE field_stats SET total_length = ?2 WHERE field = ?1")
        .map_err(sqlite_error)?;
    for (field, delta) in deltas {
        let current: i64 = select
            .query_row([field], |row| row.get(0))
            .optional()
            .map_err(sqlite_error)?
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical field statistic is missing a retired field".to_owned(),
                )
            })?;
        let next = current.checked_add(*delta).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "carried lexical field statistic overflowed".to_owned(),
            )
        })?;
        if next < 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "carried lexical field statistic became negative".to_owned(),
            ));
        }
        update.execute(params![field, next]).map_err(sqlite_error)?;
    }
    Ok(())
}

fn merge_term_runs(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "term_posting_runs")? {
        return Ok(());
    }
    let subtoken = field_code(LexicalFieldV1::Subtoken);
    let mut select = transaction
        .prepare(
            "SELECT term, field, postings FROM term_posting_runs ORDER BY term, field, page_ordinal",
        )
        .map_err(sqlite_error)?;
    let runs = select
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(select);
    let mut existing = transaction
        .prepare("SELECT lists FROM term_postings WHERE term = ?1")
        .map_err(sqlite_error)?;
    let mut upsert = transaction
        .prepare(
            "INSERT INTO term_postings(term, in_fuzzy, lists) VALUES (?1, ?2, ?3)
             ON CONFLICT(term) DO UPDATE SET in_fuzzy = excluded.in_fuzzy, lists = excluded.lists",
        )
        .map_err(sqlite_error)?;
    let mut index = 0usize;
    while index < runs.len() {
        checkpoint(control)?;
        let term = runs[index].0.clone();
        let mut fields: BTreeMap<i64, PostingListEncoderV1> = BTreeMap::new();
        if let Some(lists) = existing
            .query_row([&term], |row| row.get::<_, Vec<u8>>(0))
            .optional()
            .map_err(sqlite_error)?
        {
            for (field, _frequency, postings) in decode_term_lists(&term_lists_bytes(&lists)?)? {
                let mut encoder = PostingListEncoderV1::new(true);
                append_staged_run(&mut encoder, postings, true)?;
                fields.insert(field, encoder);
            }
        }
        while index < runs.len() && runs[index].0 == term {
            let field = runs[index].1;
            let postings = &runs[index].2;
            let encoder = fields
                .entry(field)
                .or_insert_with(|| PostingListEncoderV1::new(true));
            append_staged_run(encoder, postings, true)?;
            index += 1;
        }
        let lists = fields
            .into_iter()
            .map(|(field, encoder)| {
                let frequency = encoder.len();
                Ok((field, frequency, encoder.finish()?))
            })
            .collect::<Result<Vec<_>, CodeLexicalArtifactErrorV1>>()?;
        let in_fuzzy = lists.iter().any(|(field, _, _)| *field != subtoken);
        upsert
            .execute(params![term, in_fuzzy, encode_term_lists(&lists)?])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn merge_exact_runs(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "exact_posting_runs")? {
        return Ok(());
    }
    let mut select = transaction
        .prepare(
            "SELECT term_id, field, documents FROM exact_posting_runs ORDER BY term_id, field, page_ordinal",
        )
        .map_err(sqlite_error)?;
    let runs = select
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(select);
    let mut existing = transaction
        .prepare("SELECT documents FROM exact_postings WHERE term_id = ?1 AND field = ?2")
        .map_err(sqlite_error)?;
    let mut upsert = transaction
        .prepare(
            "INSERT INTO exact_postings(term_id, field, documents) VALUES (?1, ?2, ?3)
             ON CONFLICT(term_id, field) DO UPDATE SET documents = excluded.documents",
        )
        .map_err(sqlite_error)?;
    let mut index = 0usize;
    while index < runs.len() {
        checkpoint(control)?;
        let term_id = runs[index].0;
        let field = runs[index].1;
        let mut encoder = PostingListEncoderV1::new(false);
        if let Some(documents) = existing
            .query_row(params![term_id, field], |row| row.get::<_, Vec<u8>>(0))
            .optional()
            .map_err(sqlite_error)?
        {
            append_staged_run(&mut encoder, &documents, false)?;
        }
        while index < runs.len() && runs[index].0 == term_id && runs[index].1 == field {
            append_staged_run(&mut encoder, &runs[index].2, false)?;
            index += 1;
        }
        upsert
            .execute(params![term_id, field, encoder.finish()?])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

fn merge_row_chunks(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "row_chunk_pages")? {
        return Ok(());
    }
    transaction
        .execute_batch(
            "INSERT INTO row_chunks(chunk_id, document_id)
             SELECT chunk_id, document_id FROM row_chunk_pages ORDER BY chunk_id;",
        )
        .map_err(sqlite_error)
}

fn merge_row_dictionary(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "row_dictionary_pages")? {
        return Ok(());
    }
    let collided: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM row_dictionary_pages AS page
                JOIN row_dictionary AS sealed ON sealed.entry_id = page.entry_id
                WHERE sealed.entry != page.entry
             )",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if collided {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "carried lexical row dictionary identifier collided".to_owned(),
        ));
    }
    transaction
        .execute_batch(
            "INSERT INTO row_dictionary(entry_id, entry)
             SELECT entry_id, MIN(entry) FROM row_dictionary_pages
             WHERE entry_id NOT IN (SELECT entry_id FROM row_dictionary)
             GROUP BY entry_id;",
        )
        .map_err(sqlite_error)
}

fn merge_field_stats(transaction: &Transaction<'_>) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "field_stats_staging")? {
        return Ok(());
    }
    transaction
        .execute_batch(
            "UPDATE field_stats
             SET total_length = total_length + (
                 SELECT total_length FROM field_stats_staging
                 WHERE field_stats_staging.field = field_stats.field
             )
             WHERE field IN (SELECT field FROM field_stats_staging);
             INSERT INTO field_stats(field, total_length)
             SELECT field, total_length FROM field_stats_staging
             WHERE field NOT IN (SELECT field FROM field_stats);",
        )
        .map_err(sqlite_error)
}

fn merge_clone_fingerprints(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, "clone_fingerprint_postings_pages")? {
        return Ok(());
    }
    let mut select = transaction
        .prepare(
            "SELECT language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position
             FROM clone_fingerprint_postings_pages
             ORDER BY language, class, normalization_revision, fingerprint, occurrence_ordinal, token_position",
        )
        .map_err(sqlite_error)?;
    let rows = select
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(select);
    if rows.is_empty() {
        return Ok(());
    }
    let mut existing = transaction
        .prepare(
            "SELECT postings FROM clone_fingerprint_postings WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
        )
        .map_err(sqlite_error)?;
    let mut upsert = transaction
        .prepare(
            "INSERT INTO clone_fingerprint_postings(language, class, normalization_revision, fingerprint, posting_count, postings)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(language, class, normalization_revision, fingerprint)
             DO UPDATE SET posting_count = excluded.posting_count, postings = excluded.postings",
        )
        .map_err(sqlite_error)?;
    let mut index = 0usize;
    while index < rows.len() {
        checkpoint(control)?;
        let language = rows[index].0.clone();
        let class = rows[index].1;
        let revision = rows[index].2;
        let fingerprint = rows[index].3;
        let mut postings = Vec::new();
        if let Some(stored) = existing
            .query_row(params![language, class, revision, fingerprint], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .optional()
            .map_err(sqlite_error)?
        {
            postings = decode_fingerprint_postings(&stored)?;
        }
        while index < rows.len()
            && rows[index].0 == language
            && rows[index].1 == class
            && rows[index].2 == revision
            && rows[index].3 == fingerprint
        {
            let occurrence = u32::try_from(rows[index].4).map_err(|_| {
                CodeLexicalArtifactErrorV1::Contract(
                    "carried clone fingerprint occurrence exceeds u32".to_owned(),
                )
            })?;
            let position = u32::try_from(rows[index].5).map_err(|_| {
                CodeLexicalArtifactErrorV1::Contract(
                    "carried clone fingerprint position exceeds u32".to_owned(),
                )
            })?;
            postings.push((occurrence, position));
            index += 1;
        }
        postings.sort_by_key(|posting| *posting);
        postings.dedup();
        let count = i64::try_from(postings.len()).map_err(|_| {
            CodeLexicalArtifactErrorV1::Contract(
                "carried clone fingerprint posting count exceeds i64".to_owned(),
            )
        })?;
        upsert
            .execute(params![
                language,
                class,
                revision,
                fingerprint,
                count,
                encode_fingerprint_postings(&postings)?
            ])
            .map_err(sqlite_error)?;
    }
    Ok(())
}

struct NgramDocumentDeltaV1 {
    remove: BTreeSet<u32>,
    add: BTreeSet<u32>,
}

/// Whether patching the changed documents' n-gram keys is a smaller walk
/// than rebuilding every sealed list.
pub(super) fn carried_ngram_patch_fits(
    transaction: &Transaction<'_>,
) -> Result<bool, CodeLexicalArtifactErrorV1> {
    if !table_exists(transaction, CARRIED_RETIRED_NGRAM_TABLE)? {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "carried lexical artifact is missing its retired n-gram roster".to_owned(),
        ));
    }
    let total = count_sql(transaction, "SELECT COUNT(*) FROM row_chunks", [])?;
    if total == 0 {
        return Ok(false);
    }
    let shift = i64::try_from(read_carried_document_shift(transaction)?).map_err(|_| {
        CodeLexicalArtifactErrorV1::Contract(
            "carried lexical document shift exceeds i64".to_owned(),
        )
    })?;
    let added = count_sql(
        transaction,
        "SELECT COUNT(*) FROM row_chunks WHERE document_id >= ?1",
        [shift],
    )?;
    let retired = count_sql(
        transaction,
        &format!("SELECT COUNT(DISTINCT document_id) FROM {CARRIED_RETIRED_NGRAM_TABLE}"),
        [],
    )?;
    Ok(retired
        .saturating_add(added)
        .saturating_mul(CARRIED_NGRAM_PATCH_DOCUMENT_FACTOR)
        < total)
}

/// Remove retired documents from the n-gram lists they contributed and add
/// the documents appended after the carry shift. Lists no changed document
/// touches stay as the parent sealed them.
pub(super) fn patch_carried_ngram_postings(
    transaction: &Transaction<'_>,
    generation: &CodeGenerationId,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let deltas = carried_ngram_deltas(transaction, generation, control)?;
    let mut select = transaction
        .prepare("SELECT documents FROM ngram_postings WHERE kind = ?1 AND ngram = ?2")
        .map_err(sqlite_error)?;
    let mut update = transaction
        .prepare(
            "UPDATE ngram_postings SET document_frequency = ?3, documents = ?4 WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare(
            "INSERT INTO ngram_postings(kind, ngram, document_frequency, documents) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(sqlite_error)?;
    let mut delete = transaction
        .prepare("DELETE FROM ngram_postings WHERE kind = ?1 AND ngram = ?2")
        .map_err(sqlite_error)?;
    for (ordinal, ((kind, ngram), delta)) in deltas.iter().enumerate() {
        if ordinal.is_multiple_of(1024) {
            checkpoint(control)?;
        }
        let stored: Option<Vec<u8>> = select
            .query_row(params![kind, ngram], |row| row.get(0))
            .optional()
            .map_err(sqlite_error)?;
        if stored.is_none() && !delta.remove.is_empty() {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "carried lexical n-gram list is missing a retired document".to_owned(),
            ));
        }
        let mut documents = match &stored {
            Some(stored) => decode_document_set(stored)?,
            None => RoaringBitmap::new(),
        };
        for document in &delta.remove {
            if !documents.remove(*document) {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical n-gram list does not contain its retired document".to_owned(),
                ));
            }
        }
        for document in &delta.add {
            if !documents.insert(*document) {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical n-gram list already contains its appended document".to_owned(),
                ));
            }
        }
        if documents.is_empty() {
            delete.execute(params![kind, ngram]).map_err(sqlite_error)?;
            continue;
        }
        let frequency = i64::try_from(documents.len()).map_err(|_| {
            CodeLexicalArtifactErrorV1::Contract(
                "carried lexical n-gram document frequency exceeds i64".to_owned(),
            )
        })?;
        let sealed = seal_ngram_documents(&documents)?;
        if stored.is_some() {
            update
                .execute(params![kind, ngram, frequency, sealed])
                .map_err(sqlite_error)?;
        } else {
            insert
                .execute(params![kind, ngram, frequency, sealed])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

pub(super) fn discard_carried_ngram_plan(
    transaction: &Transaction<'_>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute_batch(&format!(
            "DROP TABLE IF EXISTS {CARRIED_RETIRED_NGRAM_TABLE};"
        ))
        .map_err(sqlite_error)
}

fn carried_ngram_deltas(
    transaction: &Transaction<'_>,
    generation: &CodeGenerationId,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<BTreeMap<(i64, i64), NgramDocumentDeltaV1>, CodeLexicalArtifactErrorV1> {
    let mut deltas: BTreeMap<(i64, i64), NgramDocumentDeltaV1> = BTreeMap::new();
    let mut retired = transaction
        .prepare(&format!(
            "SELECT kind, ngram, document_id FROM {CARRIED_RETIRED_NGRAM_TABLE} ORDER BY kind, ngram"
        ))
        .map_err(sqlite_error)?;
    let mut rows = retired.query([]).map_err(sqlite_error)?;
    let mut visited = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if visited.is_multiple_of(1024) {
            checkpoint(control)?;
        }
        visited += 1;
        let kind: i64 = row.get(0).map_err(sqlite_error)?;
        let ngram: i64 = row.get(1).map_err(sqlite_error)?;
        let document =
            u32::try_from(row.get::<_, i64>(2).map_err(sqlite_error)?).map_err(|_| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical retired n-gram document exceeds u32".to_owned(),
                )
            })?;
        deltas
            .entry((kind, ngram))
            .or_insert_with(|| NgramDocumentDeltaV1 {
                remove: BTreeSet::new(),
                add: BTreeSet::new(),
            })
            .remove
            .insert(document);
    }
    drop(rows);
    drop(retired);
    let shift = i64::try_from(read_carried_document_shift(transaction)?).map_err(|_| {
        CodeLexicalArtifactErrorV1::Contract(
            "carried lexical document shift exceeds i64".to_owned(),
        )
    })?;
    let mut blocks = transaction
        .prepare(
            "SELECT first_document, payload FROM row_blocks WHERE first_document >= ?1 ORDER BY first_document",
        )
        .map_err(sqlite_error)?;
    let stored = blocks
        .query_map(params![shift], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(blocks);
    let dictionary = ConnectionRowDictionaryV1::new(transaction);
    for (first_document, payload) in stored {
        checkpoint(control)?;
        for row in decode_row_block(first_document, &payload)? {
            if i64::from(row.document_id) < shift {
                continue;
            }
            let decoded =
                decode_artifact_row(generation, &row.chunk_id, &row.row, &row.text, &dictionary)?;
            for (kind, ngram) in document_ngram_keys(
                &super::super::normalized_search_text(&decoded),
                decoded.sanitized_text.as_str(),
                decoded.normalized_text.as_str(),
                control,
            )? {
                deltas
                    .entry((kind, ngram))
                    .or_insert_with(|| NgramDocumentDeltaV1 {
                        remove: BTreeSet::new(),
                        add: BTreeSet::new(),
                    })
                    .add
                    .insert(row.document_id);
            }
        }
    }
    Ok(deltas)
}

fn seal_ngram_documents(documents: &RoaringBitmap) -> Result<Vec<u8>, CodeLexicalArtifactErrorV1> {
    let mut encoder = PostingListEncoderV1::new(false);
    for document in documents {
        encoder.push(document, 1)?;
    }
    encoder.finish_document_set()
}

fn count_sql(
    transaction: &Transaction<'_>,
    sql: &str,
    params: impl Params,
) -> Result<i64, CodeLexicalArtifactErrorV1> {
    transaction
        .query_row(sql, params, |row| row.get(0))
        .map_err(sqlite_error)
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, CodeLexicalArtifactErrorV1> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(sqlite_error)
}
