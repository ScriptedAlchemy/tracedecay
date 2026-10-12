//! Ordered SQLite appends of admitted page rows, clones, and posting runs.

use super::super::clone_codec::{digest_from_key, digest_key};
use super::super::format::{PostingListEncoderV1, contract_number};
use super::super::prepared::{PreparedCloneBodyV1, PreparedCodeLexicalArtifactPageV1};
use super::super::row_codec::{stored_chunk_key, stored_symbol_key};
use super::super::schema::{field_code, intern_exact_terms};
use super::super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_error};
use super::admission::{
    EXACT_INSERT_CONTROL_INTERVAL, INSERT_ROWS_PER_STATEMENT, PreparedExactInsertPlanV1,
    PreparedTermInsertPlanV1, TERM_INSERT_CONTROL_INTERVAL,
};
use rusqlite::types::{ToSqlOutput, Value, ValueRef};
use rusqlite::{Transaction, params};
use std::collections::{BTreeMap, HashMap};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::ManifestDigest;

/// Buffers rows for one base table and flushes them as multi-row `INSERT`
/// statements: full statements of [`INSERT_ROWS_PER_STATEMENT`] rows while
/// rows keep arriving, one shorter statement for the remainder at `finish`.
/// Row order is preserved, so the clustered-key insert plans still append
/// at the tail of their trees.
struct MultiRowInsertV1<'transaction, 'row> {
    transaction: &'transaction Transaction<'transaction>,
    table_columns: &'static str,
    columns: usize,
    conflict_clause: &'static str,
    full_statement: rusqlite::CachedStatement<'transaction>,
    buffer: Vec<ToSqlOutput<'row>>,
    map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
}

impl<'transaction, 'row> MultiRowInsertV1<'transaction, 'row> {
    fn new(
        transaction: &'transaction Transaction<'transaction>,
        table_columns: &'static str,
        columns: usize,
        map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Self::new_with_conflict_clause(transaction, table_columns, columns, "", map_error)
    }

    /// Same as [`Self::new`], but every flushed statement carries
    /// `conflict_clause` (e.g. `" ON CONFLICT(payload_digest) DO NOTHING"`)
    /// after its `VALUES (...)` tuples, for tables whose rows may
    /// legitimately repeat an existing key (content-addressed dedup) rather
    /// than signal a bug.
    fn new_with_conflict_clause(
        transaction: &'transaction Transaction<'transaction>,
        table_columns: &'static str,
        columns: usize,
        conflict_clause: &'static str,
        map_error: fn(rusqlite::Error) -> CodeLexicalArtifactErrorV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let full_statement = transaction
            .prepare_cached(&multi_row_insert_sql(
                table_columns,
                columns,
                INSERT_ROWS_PER_STATEMENT,
                conflict_clause,
            ))
            .map_err(map_error)?;
        Ok(Self {
            transaction,
            table_columns,
            columns,
            conflict_clause,
            full_statement,
            buffer: Vec::with_capacity(columns * INSERT_ROWS_PER_STATEMENT),
            map_error,
        })
    }

    fn push(
        &mut self,
        values: impl IntoIterator<Item = ToSqlOutput<'row>>,
    ) -> Result<(), CodeLexicalArtifactErrorV1> {
        let before = self.buffer.len();
        self.buffer.extend(values);
        if self.buffer.len() != before + self.columns {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact multi-row insert received the wrong column count".to_owned(),
            ));
        }
        if self.buffer.len() == self.columns * INSERT_ROWS_PER_STATEMENT {
            self.full_statement
                .execute(rusqlite::params_from_iter(self.buffer.iter()))
                .map_err(self.map_error)?;
            self.buffer.clear();
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), CodeLexicalArtifactErrorV1> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let rows = self.buffer.len() / self.columns;
        let mut tail = self
            .transaction
            .prepare_cached(&multi_row_insert_sql(
                self.table_columns,
                self.columns,
                rows,
                self.conflict_clause,
            ))
            .map_err(self.map_error)?;
        tail.execute(rusqlite::params_from_iter(self.buffer.iter()))
            .map_err(self.map_error)?;
        self.buffer.clear();
        Ok(())
    }
}

fn multi_row_insert_sql(
    table_columns: &str,
    columns: usize,
    rows: usize,
    conflict_clause: &str,
) -> String {
    let tuple = format!(
        "({})",
        std::iter::repeat_n("?", columns)
            .collect::<Vec<_>>()
            .join(", ")
    );
    format!(
        "INSERT INTO {table_columns} VALUES {}{conflict_clause}",
        std::iter::repeat_n(tuple.as_str(), rows)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sql_integer<'row>(value: i64) -> ToSqlOutput<'row> {
    ToSqlOutput::Owned(Value::Integer(value))
}

fn sql_text(value: &str) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes()))
}

fn sql_blob(value: &[u8]) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(ValueRef::Blob(value))
}

/// Digests checked against on-disk `clone_body_payloads` content in one
/// `IN (...)` query per chunk, instead of one `SELECT` per body. Five
/// columns of overhead at [`INSERT_ROWS_PER_STATEMENT`] rows stays well
/// clear of SQLite's 999-parameter floor for a single-column `IN` list too.
pub(super) const PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK: usize = 512;

/// Verify that every payload digest about to be staged in this batch
/// agrees, byte for byte, with the payload already recorded under that
/// digest. Either earlier in this same batch (checked in memory, no I/O)
/// or in a prior batch already committed to `clone_body_payloads` (checked
/// with one batched `SELECT ... WHERE payload_digest IN (...)` per chunk of
/// unique digests, rather than a `SELECT` after every single insert). A
/// fresh digest with no prior occurrence anywhere needs no read at all: it
/// cannot conflict with content that was never stored.
pub(super) fn verify_clone_payload_digests<'body>(
    transaction: &Transaction<'_>,
    bodies: &[&'body PreparedCloneBodyV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut staged: HashMap<&'body str, &'body [u8]> = HashMap::with_capacity(bodies.len());
    for body in bodies {
        checkpoint(control)?;
        match staged.entry(body.payload_digest.as_str()) {
            std::collections::hash_map::Entry::Occupied(existing) => {
                if *existing.get() != body.payload.as_slice() {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(
                        "clone payload digest collision".to_owned(),
                    ));
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(body.payload.as_slice());
            }
        }
    }
    let digests: Vec<&str> = staged.keys().copied().collect();
    let placeholders = std::iter::repeat_n("?", PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK)
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT payload_digest, payload FROM clone_body_payloads WHERE payload_digest IN ({placeholders})"
    );
    let mut statement = transaction.prepare_cached(&sql).map_err(sqlite_error)?;
    for chunk in digests.chunks(PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK) {
        checkpoint(control)?;
        let keys = chunk
            .iter()
            .map(|digest| stored_digest_key(digest))
            .collect::<Result<Vec<_>, _>>()?;
        let parameters = keys.iter().map(Some).chain(std::iter::repeat_n(
            None,
            PAYLOAD_DIGEST_CONFLICT_CHECK_CHUNK - chunk.len(),
        ));
        let mut rows = statement
            .query(rusqlite::params_from_iter(parameters))
            .map_err(sqlite_error)?;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            let key: Vec<u8> = row.get(0).map_err(sqlite_error)?;
            let digest = digest_from_key(&key)?;
            let stored: Vec<u8> = row.get(1).map_err(sqlite_error)?;
            if let Some(expected) = staged.get(digest.as_str())
                && *expected != stored.as_slice()
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone payload digest collision".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn append_prepared_clone_bodies(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let bodies: Vec<_> = pages
        .iter()
        .flat_map(|page| page.clone_bodies.iter())
        .collect();
    if !bodies.is_empty() {
        verify_clone_payload_digests(transaction, &bodies, control)?;
        let payload_keys = bodies
            .iter()
            .map(|body| stored_digest_key(&body.payload_digest))
            .collect::<Result<Vec<_>, _>>()?;
        let symbol_keys = bodies
            .iter()
            .map(|body| stored_symbol_key(&body.symbol_occurrence_id))
            .collect::<Vec<_>>();
        let exact_digests = bodies
            .iter()
            .map(|body| {
                body.exact_keys
                    .iter()
                    .map(|key| digest_key(&key.digest))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut payload_insert = MultiRowInsertV1::new_with_conflict_clause(
            transaction,
            "clone_body_payloads(payload_digest, payload)",
            2,
            " ON CONFLICT(payload_digest) DO NOTHING",
            sqlite_error,
        )?;
        for (body, key) in bodies.iter().zip(&payload_keys) {
            checkpoint(control)?;
            payload_insert.push([sql_blob(key), sql_blob(&body.payload)])?;
        }
        payload_insert.finish()?;
        // Occurrences name their payload, and postings their occurrence, by
        // the ordinal those rows were just assigned.
        let mut payload_ordinal = transaction
            .prepare_cached("SELECT ordinal FROM clone_body_payloads WHERE payload_digest = ?1")
            .map_err(sqlite_error)?;
        let mut occurrence_insert = MultiRowInsertV1::new(
            transaction,
            "clone_occurrences(symbol_key, payload_ordinal, path, body_start, body_end, eligibility)",
            6,
            sqlite_error,
        )?;
        for ((body, key), symbol_key) in bodies.iter().zip(&payload_keys).zip(&symbol_keys) {
            checkpoint(control)?;
            let ordinal: i64 = payload_ordinal
                .query_row([key.as_slice()], |row| row.get(0))
                .map_err(sqlite_error)?;
            occurrence_insert.push([
                ToSqlOutput::Borrowed(symbol_key.into()),
                sql_integer(ordinal),
                sql_text(&body.path),
                sql_integer(i64::try_from(body.body_start).map_err(contract_number)?),
                sql_integer(i64::try_from(body.body_end).map_err(contract_number)?),
                sql_blob(&body.eligibility),
            ])?;
        }
        occurrence_insert.finish()?;
        let mut occurrence_ordinal = transaction
            .prepare_cached("SELECT ordinal FROM clone_occurrences WHERE symbol_key = ?1")
            .map_err(sqlite_error)?;
        let mut posting_insert = MultiRowInsertV1::new(
            transaction,
            "clone_exact_postings(class, normalization_revision, digest, occurrence_ordinal)",
            4,
            sqlite_error,
        )?;
        for ((body, symbol_key), digests) in bodies.iter().zip(&symbol_keys).zip(&exact_digests) {
            checkpoint(control)?;
            if body.exact_keys.is_empty() {
                continue;
            }
            let ordinal: i64 = occurrence_ordinal
                .query_row([symbol_key], |row| row.get(0))
                .map_err(sqlite_error)?;
            for (key, digest) in body.exact_keys.iter().zip(digests) {
                posting_insert.push([
                    sql_integer(i64::from(key.class as u8)),
                    sql_integer(i64::from(key.normalization_revision)),
                    sql_blob(digest),
                    sql_integer(ordinal),
                ])?;
            }
        }
        posting_insert.finish()?;
    }
    Ok(())
}

/// The key `clone_body_payloads.payload_digest` stores for `digest`.
pub(in super::super) fn stored_digest_key(
    digest: &str,
) -> Result<[u8; 32], CodeLexicalArtifactErrorV1> {
    digest_key(
        &ManifestDigest::new(digest.to_owned())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
    )
}

/// Append one posting list per `(term, field)` and per `(exact term, field)`
/// covering this batch, keyed by the batch's first page so every batch lands
/// at the tail of its run table. Finalization concatenates the runs of each
/// key in page order; n-gram lists are rebuilt from the stored rows instead.
pub(super) fn append_prepared_postings(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    term_insert_plan: &PreparedTermInsertPlanV1<'_>,
    exact_insert_plan: &PreparedExactInsertPlanV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let batch_page = pages
        .first()
        .map(|page| i64::try_from(page.page_ordinal).map_err(contract_number))
        .transpose()?
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract(
                "lexical artifact posting batch has no pages".to_owned(),
            )
        })?;
    {
        let _span = tracing::trace_span!("query.artifact.batch.postings.intern_exact").entered();
        intern_exact_terms(transaction, &exact_insert_plan.interned_terms, control)
    }?;
    let mut term_insert = MultiRowInsertV1::new(
        transaction,
        "term_posting_runs(page_ordinal, term, field, postings)",
        4,
        sqlite_error,
    )?;
    // Plain INSERT, not `INSERT OR IGNORE`: every prepared document's exact
    // postings are deduplicated into a `BTreeSet<(field, term)>`
    // (`prepared.rs::prepare_document`), and `validate_prepared_page_batch`
    // admits only the fresh contiguous suffix of pages, so each
    // `(batch page, term, field)` run key is globally unique and a conflict
    // can only be a real corruption bug.
    let mut exact_insert = MultiRowInsertV1::new(
        transaction,
        "exact_posting_runs(page_ordinal, term_id, field, documents)",
        4,
        sqlite_error,
    )?;
    let mut field_totals = BTreeMap::new();
    {
        let _span = tracing::trace_span!("query.artifact.batch.postings.term_rows").entered();
        {
            let mut run: Option<((&str, i64), PostingListEncoderV1)> = None;
            for (index, entry) in term_insert_plan.entries.iter().enumerate() {
                if index.is_multiple_of(TERM_INSERT_CONTROL_INTERVAL) {
                    checkpoint(control)?;
                }
                let (term, field, document_id) = entry.key();
                if let Some(((term, field), encoder)) =
                    run.take_if(|(key, _)| *key != (term, field))
                {
                    push_posting_run(&mut term_insert, batch_page, sql_text(term), field, encoder)?;
                }
                run.get_or_insert_with(|| ((term, field), PostingListEncoderV1::new(true)))
                    .1
                    .push(
                        u32::try_from(document_id).map_err(contract_number)?,
                        u32::try_from(entry.posting.frequency).map_err(contract_number)?,
                    )?;
                let total: &mut i64 = field_totals.entry(field).or_default();
                *total = total.checked_add(entry.posting.frequency).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "lexical artifact field total overflowed".to_owned(),
                    )
                })?;
            }
            if let Some(((term, field), encoder)) = run {
                push_posting_run(&mut term_insert, batch_page, sql_text(term), field, encoder)?;
            }
            term_insert.finish()
        }
    }?;
    {
        let _span = tracing::trace_span!("query.artifact.batch.postings.field_totals").entered();
        stage_field_totals(transaction, &field_totals)
    }?;
    {
        let _span = tracing::trace_span!("query.artifact.batch.postings.exact_rows").entered();
        {
            let mut run: Option<((i64, i64), PostingListEncoderV1)> = None;
            for (index, entry) in exact_insert_plan.entries.iter().enumerate() {
                if index.is_multiple_of(EXACT_INSERT_CONTROL_INTERVAL) {
                    checkpoint(control)?;
                }
                let key = (entry.term_id, entry.field_code);
                if let Some(((term_id, field), encoder)) =
                    run.take_if(|(run_key, _)| *run_key != key)
                {
                    push_posting_run(
                        &mut exact_insert,
                        batch_page,
                        sql_integer(term_id),
                        field,
                        encoder,
                    )?;
                }
                run.get_or_insert_with(|| (key, PostingListEncoderV1::new(false)))
                    .1
                    .push(
                        u32::try_from(entry.document_id).map_err(contract_number)?,
                        1,
                    )?;
            }
            if let Some(((term_id, field), encoder)) = run {
                push_posting_run(
                    &mut exact_insert,
                    batch_page,
                    sql_integer(term_id),
                    field,
                    encoder,
                )?;
            }
            exact_insert.finish()
        }
    }?;
    Ok(())
}

fn push_posting_run<'a>(
    insert: &mut MultiRowInsertV1<'_, 'a>,
    batch_page: i64,
    term: ToSqlOutput<'a>,
    field: i64,
    encoder: PostingListEncoderV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    insert.push([
        sql_integer(batch_page),
        term,
        sql_integer(field),
        ToSqlOutput::Owned(Value::Blob(encoder.finish()?)),
    ])
}

pub(super) fn append_prepared_rows(
    transaction: &Transaction<'_>,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut block_insert = MultiRowInsertV1::new(
        transaction,
        "row_blocks(first_document, payload)",
        2,
        |error| CodeLexicalArtifactErrorV1::Contract(error.to_string()),
    )?;
    for page in pages {
        for (first_document, payload) in &page.row_blocks {
            checkpoint(control)?;
            block_insert.push([sql_integer(*first_document), sql_blob(payload)])?;
        }
    }
    block_insert.finish()?;
    let mut chunk_insert = MultiRowInsertV1::new(
        transaction,
        "row_chunk_pages(document_id, chunk_id)",
        2,
        |error| CodeLexicalArtifactErrorV1::Contract(error.to_string()),
    )?;
    for page in pages {
        for document in &page.documents {
            checkpoint(control)?;
            chunk_insert.push([
                sql_integer(document.document_id),
                ToSqlOutput::Owned(stored_chunk_key(&document.chunk_id)),
            ])?;
        }
    }
    chunk_insert.finish()
}

pub(super) fn insert_prepared_source_page(
    transaction: &Transaction<'_>,
    page: &PreparedCodeLexicalArtifactPageV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let page_ordinal = i64::try_from(page.page_ordinal).map_err(contract_number)?;
    transaction
        .prepare_cached(
            "INSERT INTO source_pages(page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .map_err(sqlite_error)?
        .execute(params![
            page_ordinal,
            i64::try_from(page.file_ordinal).map_err(contract_number)?,
            i64::try_from(page.chunk_count).map_err(contract_number)?,
            i64::try_from(page.payload_bytes).map_err(contract_number)?,
            i64::try_from(page.import_count).map_err(contract_number)?,
            i64::try_from(page.import_payload_bytes).map_err(contract_number)?,
            page.import_digest.as_str(),
            i64::try_from(page.clone_bodies.len()).map_err(contract_number)?,
            page.ngram_digest.as_str(),
            page.base_sections_receipt.as_slice(),
        ])
        .map_err(sqlite_error)?;
    transaction
        .prepare_cached(
            "INSERT INTO source_page_cursors(page_ordinal, page_digest, cumulative_digest, payload_bytes, next_cursor) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(sqlite_error)?
        .execute(params![
            page_ordinal,
            page.page_digest.as_str(),
            page.cumulative_digest.as_str(),
            i64::try_from(page.payload_bytes).map_err(contract_number)?,
            page.next_cursor.as_slice(),
        ])
        .map_err(sqlite_error)?;
    Ok(())
}

/// Fold one committed batch's per-field posting lengths into
/// `field_stats_staging`, inside that batch's transaction, so sealing
/// `field_stats` never rescans `term_postings`.
pub(super) fn stage_field_totals(
    transaction: &Transaction<'_>,
    totals: &BTreeMap<i64, i64>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut upsert = transaction
        .prepare_cached(
            "INSERT INTO field_stats_staging(field, total_length) VALUES (?1, ?2)
             ON CONFLICT(field) DO UPDATE SET total_length = total_length + excluded.total_length",
        )
        .map_err(sqlite_error)?;
    for (field, total) in totals {
        upsert.execute([*field, *total]).map_err(sqlite_error)?;
    }
    Ok(())
}
