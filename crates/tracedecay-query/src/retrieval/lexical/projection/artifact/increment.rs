//! Carry a sealed parent artifact into a successor that re-encodes only the
//! pages of the files that changed.
//!
//! Pages never span files and their stored receipts are position-free, so
//! every unchanged file's rows, postings, receipts, and clone rows are
//! already the rows a cold build of the successor writes, at document,
//! page, and occurrence positions shifted by the changed files' growth.
//! The carry replaces each changed file's pages in place at the cold
//! position, shifts what follows, and leaves the staging file exactly where
//! a cold build stands when it enters digest verification. The digests,
//! layout rewrite, and receipt are then the cold build's own steps, which
//! is what makes the sealed bytes equal.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction, params};
use tracedecay_code_index::production::{CodeIndexExecutionControlV1, VerifiedSealedLexicalPageV1};
use tracedecay_domain::CodeGenerationId;

use super::builder::page_transient_peak_bytes;
use super::format::contract_number;
use super::prepared::{PreparedCodeLexicalArtifactPageV1, prepare_page};
use super::row_codec::{
    ConnectionRowDictionaryV1, RowDictionaryEntryV1, RowDictionaryV1, decode_artifact_row,
    decode_row_block, stored_chunk_key,
};
use super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_error};
use crate::retrieval::lexical::projection::CodeLexicalProjectionMetadataV1;

mod clones;
mod postings;

use clones::carry_clone_rows;
use postings::{carry_exact_postings, carry_ngram_postings, carry_term_postings};

/// Rows one bounded read of a serving table holds before its writes apply.
const CARRY_SCAN_ROWS: usize = 4_096;

/// Stages one changed file's pages as a cold build that reached it at
/// `(file ordinal, first page ordinal, first chunk)` would mint them.
pub type CarriedFilePagesV1<'a> = dyn FnMut(u64, u64, u64) -> Result<Vec<VerifiedSealedLexicalPageV1>, CodeLexicalArtifactErrorV1>
    + 'a;

/// What the carry wrote, for the content epoch and the cost counters.
pub(super) struct CarriedRowsV1 {
    pub(super) pages: u64,
    pub(super) documents: u64,
    pub(super) re_encoded_pages: u64,
    pub(super) carried_pages: u64,
}

/// One stored `source_pages` row; position-free apart from its key.
#[derive(Clone)]
struct PageRowV1 {
    file_ordinal: i64,
    chunk_count: i64,
    payload_bytes: i64,
    import_count: i64,
    import_payload_bytes: i64,
    import_digest: String,
    clone_body_count: i64,
    ngram_digest: String,
    base_sections_receipt: Vec<u8>,
}

impl PageRowV1 {
    fn of_prepared(
        page: &PreparedCodeLexicalArtifactPageV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        Ok(Self {
            file_ordinal: i64::try_from(page.file_ordinal).map_err(contract_number)?,
            chunk_count: i64::try_from(page.chunk_count).map_err(contract_number)?,
            payload_bytes: i64::try_from(page.payload_bytes).map_err(contract_number)?,
            import_count: i64::try_from(page.import_count).map_err(contract_number)?,
            import_payload_bytes: i64::try_from(page.import_payload_bytes)
                .map_err(contract_number)?,
            import_digest: page.import_digest.as_str().to_owned(),
            clone_body_count: i64::try_from(page.clone_bodies.len()).map_err(contract_number)?,
            ngram_digest: page.ngram_digest.as_str().to_owned(),
            base_sections_receipt: page.base_sections_receipt.clone(),
        })
    }
}

/// One changed file: the parent positions it held and the pages it now has.
struct ChangedFileV1 {
    old_pages: (usize, usize),
    old_documents: (u64, u64),
    old_clones: (u64, u64),
    pages: Vec<PreparedCodeLexicalArtifactPageV1>,
}

/// Maps a parent position (document id, page ordinal, or clone ordinal) to
/// the successor's: positions inside a replaced range have no image, and
/// every other position moves by the growth of the replaced ranges before
/// it.
struct ShiftV1 {
    /// Replaced parent ranges `[start, end)`, ascending and disjoint.
    replaced: Vec<(u64, u64)>,
    /// Successor minus parent length summed over `replaced[..=i]`.
    growth: Vec<i64>,
}

impl ShiftV1 {
    fn new(
        ranges: impl IntoIterator<Item = ((u64, u64), u64)>,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let mut replaced = Vec::new();
        let mut growth = Vec::new();
        let mut total = 0i64;
        for ((start, end), successor_length) in ranges {
            if replaced
                .last()
                .is_some_and(|(_, previous_end)| *previous_end > start)
                || end < start
            {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical ranges overlap".to_owned(),
                ));
            }
            let parent_length = i64::try_from(end - start).map_err(contract_number)?;
            total = total
                .checked_add(i64::try_from(successor_length).map_err(contract_number)?)
                .and_then(|total| total.checked_sub(parent_length))
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Contract(
                        "carried lexical growth overflowed".to_owned(),
                    )
                })?;
            replaced.push((start, end));
            growth.push(total);
        }
        Ok(Self { replaced, growth })
    }

    fn map(&self, parent: u64) -> Result<Option<u64>, CodeLexicalArtifactErrorV1> {
        let before = self.replaced.partition_point(|(_, end)| *end <= parent);
        if self
            .replaced
            .get(before)
            .is_some_and(|(start, end)| *start <= parent && parent < *end)
        {
            return Ok(None);
        }
        let growth = before.checked_sub(1).map_or(0, |index| self.growth[index]);
        i64::try_from(parent)
            .ok()
            .and_then(|parent| parent.checked_add(growth))
            .and_then(|mapped| u64::try_from(mapped).ok())
            .map(Some)
            .ok_or_else(|| {
                CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical position underflowed".to_owned(),
                )
            })
    }

    fn map_u32(&self, parent: u32) -> Result<Option<u32>, CodeLexicalArtifactErrorV1> {
        self.map(u64::from(parent))?
            .map(|mapped| u32::try_from(mapped).map_err(contract_number))
            .transpose()
    }

    /// Whether any carried position moves; otherwise only the replaced
    /// ranges change.
    fn shifts(&self) -> bool {
        self.growth.iter().any(|growth| *growth != 0)
    }

    /// The first parent position that is replaced or moves.
    fn first_affected(&self) -> Option<u64> {
        self.replaced.first().map(|(start, _)| *start)
    }

    /// Whether any of `positions` (ascending) is replaced or moves. Reading
    /// stops past the last replaced range when nothing after it moves.
    fn moves_any(
        &self,
        positions: impl Iterator<Item = Result<(u32, u32), CodeLexicalArtifactErrorV1>>,
    ) -> Result<bool, CodeLexicalArtifactErrorV1> {
        let Some(first) = self.first_affected() else {
            return Ok(false);
        };
        let settled = self
            .growth
            .last()
            .filter(|growth| **growth == 0)
            .and(self.replaced.last().map(|(_, end)| *end));
        for position in positions {
            let position = u64::from(position?.0);
            if position < first {
                continue;
            }
            if settled.is_some_and(|settled| position >= settled) {
                return Ok(false);
            }
            if self.map(position)? != Some(position) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a set holds a replaced or moved position, asking only
    /// `any_in(start, end)`: does it hold a position in `[start, end)`.
    fn moves_any_in(&self, any_in: impl Fn(u64, u64) -> bool) -> bool {
        self.replaced
            .iter()
            .enumerate()
            .any(|(index, (start, end))| {
                let next = self
                    .replaced
                    .get(index + 1)
                    .map_or(u64::MAX, |(next_start, _)| *next_start);
                any_in(*start, *end) || (self.growth[index] != 0 && any_in(*end, next))
            })
    }

    /// An SQL expression giving `column`'s successor value for a carried
    /// row; it reads only the parent value, so one `UPDATE` applies it.
    fn sql_image(&self, column: &str) -> String {
        let mut expression = String::from("CASE");
        for ((_, end), growth) in self.replaced.iter().zip(&self.growth).rev() {
            expression.push_str(&format!(
                " WHEN {column} >= {end} THEN {column} + ({growth})"
            ));
        }
        expression.push_str(&format!(" ELSE {column} END"));
        expression
    }

    fn sql_replaced(&self, column: &str) -> String {
        if self.replaced.is_empty() {
            return "0".to_owned();
        }
        self.replaced
            .iter()
            .map(|(start, end)| format!("({column} >= {start} AND {column} < {end})"))
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}

/// Records every dictionary entry a decoded row references.
struct RecordingRowDictionaryV1<'a> {
    inner: ConnectionRowDictionaryV1<'a>,
    referenced: RefCell<BTreeSet<i64>>,
}

impl RowDictionaryV1 for RecordingRowDictionaryV1<'_> {
    fn entry(
        &self,
        entry_id: i64,
    ) -> Result<Arc<RowDictionaryEntryV1>, CodeLexicalArtifactErrorV1> {
        self.referenced.borrow_mut().insert(entry_id);
        self.inner.entry(entry_id)
    }
}

/// Replace the changed files' pages of the sealed parent copy `transaction`
/// holds with the successor's, in place, and shift everything after them.
#[tracing::instrument(name = "query.artifact.carry.patch", level = "trace", skip_all)]
pub(super) fn carry_parent_rows(
    transaction: &Transaction<'_>,
    metadata: &CodeLexicalProjectionMetadataV1,
    changed_files: &[u64],
    stage_file_pages: &mut CarriedFilePagesV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CarriedRowsV1, CodeLexicalArtifactErrorV1> {
    let parent_pages = read_page_rows(transaction, control)?;
    let changed = stage_changed_files(
        metadata,
        &parent_pages,
        changed_files,
        stage_file_pages,
        control,
    )?;
    let documents = ShiftV1::new(changed.iter().map(|file| {
        (
            file.old_documents,
            file.pages.iter().map(|page| page.chunk_count).sum::<u64>(),
        )
    }))?;
    // Occurrence ordinals are rowids, so the k-th clone body is ordinal k+1.
    let clones = ShiftV1::new(changed.iter().map(|file| {
        (
            (file.old_clones.0 + 1, file.old_clones.1 + 1),
            file.pages
                .iter()
                .map(|page| page.clone_bodies.len() as u64)
                .sum::<u64>(),
        )
    }))?;
    let new_pages: Vec<&PreparedCodeLexicalArtifactPageV1> =
        changed.iter().flat_map(|file| &file.pages).collect();
    metrics::gauge!("query.artifact.carry.document_growth").set(
        documents
            .growth
            .last()
            .map_or(0, |growth| growth.unsigned_abs()) as f64,
    );
    {
        let _span = tracing::trace_span!("query.artifact.carry.row_dictionary").entered();
        carry_row_dictionary(transaction, metadata, &parent_pages, &changed, control)
    }?;
    {
        let _span = tracing::trace_span!("query.artifact.carry.rows").entered();
        carry_row_blocks(transaction, &documents, &new_pages, control)?;
        carry_row_chunks(transaction, &documents, &new_pages, control)
    }?;
    let mut field_totals = BTreeMap::<i64, i64>::new();
    {
        let _span = tracing::trace_span!("query.artifact.carry.term_postings").entered();
        carry_term_postings(
            transaction,
            &documents,
            &new_pages,
            &mut field_totals,
            control,
        )
    }?;
    apply_field_totals(transaction, &field_totals)?;
    {
        let _span = tracing::trace_span!("query.artifact.carry.exact_postings").entered();
        carry_exact_postings(transaction, &documents, &new_pages, control)
    }?;
    {
        let _span = tracing::trace_span!("query.artifact.carry.ngram_postings").entered();
        carry_ngram_postings(transaction, &documents, &new_pages, control)
    }?;
    {
        let _span = tracing::trace_span!("query.artifact.carry.clones").entered();
        carry_clone_rows(transaction, &clones, &changed, control)
    }?;
    let page_count = write_page_rows(transaction, &parent_pages, &changed)?;
    let documents: i64 = transaction
        .query_row("SELECT COUNT(*) FROM row_chunks", [], |row| row.get(0))
        .map_err(sqlite_error)?;
    let re_encoded_pages = new_pages.len() as u64;
    metrics::gauge!("query.artifact.carry.pages_encoded").increment(re_encoded_pages as f64);
    metrics::gauge!("query.artifact.carry.files_re_encoded").increment(changed.len() as f64);
    Ok(CarriedRowsV1 {
        pages: page_count,
        documents: u64::try_from(documents).map_err(contract_number)?,
        re_encoded_pages,
        carried_pages: page_count - re_encoded_pages,
    })
}

fn read_page_rows(
    transaction: &Transaction<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<PageRowV1>, CodeLexicalArtifactErrorV1> {
    let mut statement = transaction
        .prepare(
            "SELECT page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt FROM source_pages ORDER BY page_ordinal",
        )
        .map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut pages = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if pages.len().is_multiple_of(CARRY_SCAN_ROWS) {
            checkpoint(control)?;
        }
        let ordinal: i64 = row.get(0).map_err(sqlite_error)?;
        let page = PageRowV1 {
            file_ordinal: row.get(1).map_err(sqlite_error)?,
            chunk_count: row.get(2).map_err(sqlite_error)?,
            payload_bytes: row.get(3).map_err(sqlite_error)?,
            import_count: row.get(4).map_err(sqlite_error)?,
            import_payload_bytes: row.get(5).map_err(sqlite_error)?,
            import_digest: row.get(6).map_err(sqlite_error)?,
            clone_body_count: row.get(7).map_err(sqlite_error)?,
            ngram_digest: row.get(8).map_err(sqlite_error)?,
            base_sections_receipt: row.get(9).map_err(sqlite_error)?,
        };
        if usize::try_from(ordinal).ok() != Some(pages.len())
            || pages
                .last()
                .is_some_and(|previous: &PageRowV1| previous.file_ordinal > page.file_ordinal)
            || page.chunk_count < 0
            || page.clone_body_count < 0
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "carried lexical parent pages are not in file order".to_owned(),
            ));
        }
        pages.push(page);
    }
    Ok(pages)
}

/// Locate each changed file's parent pages and stage its successor pages at
/// the position a cold build reaches it.
fn stage_changed_files(
    metadata: &CodeLexicalProjectionMetadataV1,
    parent_pages: &[PageRowV1],
    changed_files: &[u64],
    stage_file_pages: &mut CarriedFilePagesV1<'_>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<ChangedFileV1>, CodeLexicalArtifactErrorV1> {
    let mut document_starts = Vec::with_capacity(parent_pages.len() + 1);
    let mut clone_starts = Vec::with_capacity(parent_pages.len() + 1);
    let (mut documents, mut clones) = (0u64, 0u64);
    for page in parent_pages {
        document_starts.push(documents);
        clone_starts.push(clones);
        documents += u64::try_from(page.chunk_count).map_err(contract_number)?;
        clones += u64::try_from(page.clone_body_count).map_err(contract_number)?;
    }
    document_starts.push(documents);
    clone_starts.push(clones);
    let mut page_growth = 0i64;
    let mut document_growth = 0i64;
    let mut previous = None;
    let mut changed = Vec::with_capacity(changed_files.len());
    for &file in changed_files {
        checkpoint(control)?;
        if previous.is_some_and(|previous| previous >= file) {
            return Err(CodeLexicalArtifactErrorV1::Contract(
                "carried lexical changed files must ascend".to_owned(),
            ));
        }
        previous = Some(file);
        let file_ordinal = i64::try_from(file).map_err(contract_number)?;
        let start = parent_pages.partition_point(|page| page.file_ordinal < file_ordinal);
        let end = parent_pages.partition_point(|page| page.file_ordinal <= file_ordinal);
        let first_page = shifted(start as u64, page_growth)?;
        let first_chunk = shifted(document_starts[start], document_growth)?;
        let staged = stage_file_pages(file, first_page, first_chunk)?;
        let mut pages = Vec::with_capacity(staged.len());
        for (offset, page) in staged.iter().enumerate() {
            if page.file_ordinal() != file || page.page_ordinal() != first_page + offset as u64 {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "carried lexical file pages are not that file's contiguous pages".to_owned(),
                ));
            }
            let scratch = page_transient_peak_bytes(metadata, page, usize::MAX)?;
            pages.push(prepare_page(metadata, page, None, scratch, control)?);
        }
        page_growth += staged.len() as i64 - (end - start) as i64;
        document_growth += pages
            .iter()
            .map(|page| page.chunk_count as i64)
            .sum::<i64>()
            - (document_starts[end] - document_starts[start]) as i64;
        changed.push(ChangedFileV1 {
            old_pages: (start, end),
            old_documents: (document_starts[start], document_starts[end]),
            old_clones: (clone_starts[start], clone_starts[end]),
            pages,
        });
    }
    Ok(changed)
}

fn shifted(position: u64, growth: i64) -> Result<u64, CodeLexicalArtifactErrorV1> {
    i64::try_from(position)
        .ok()
        .and_then(|position| position.checked_add(growth))
        .and_then(|position| u64::try_from(position).ok())
        .ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Corrupt("carried lexical position underflowed".to_owned())
        })
}

/// Count each entry once per page that names it: the replaced pages' rows
/// are decoded to learn what they named, the successor pages bring their
/// own dictionaries, and an entry no page names any more is retired.
fn carry_row_dictionary(
    transaction: &Transaction<'_>,
    metadata: &CodeLexicalProjectionMetadataV1,
    parent_pages: &[PageRowV1],
    changed: &[ChangedFileV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut net = BTreeMap::<i64, i64>::new();
    let mut entries = BTreeMap::<i64, &[u8]>::new();
    let mut page_start = 0u64;
    let mut starts = Vec::with_capacity(parent_pages.len());
    for page in parent_pages {
        starts.push(page_start);
        page_start += page.chunk_count as u64;
    }
    for file in changed {
        for page in file.old_pages.0..file.old_pages.1 {
            checkpoint(control)?;
            let first = starts[page];
            let end = first + parent_pages[page].chunk_count as u64;
            for entry in
                parent_page_dictionary(transaction, &metadata.generation, first, end, control)?
            {
                *net.entry(entry).or_default() -= 1;
            }
        }
        for page in &file.pages {
            for (entry_id, entry) in &page.row_dictionary {
                *net.entry(*entry_id).or_default() += 1;
                entries.insert(*entry_id, entry.as_slice());
            }
        }
    }
    let mut stored = transaction
        .prepare_cached("SELECT entry, page_references FROM row_dictionary WHERE entry_id = ?1")
        .map_err(sqlite_error)?;
    for (entry_id, delta) in net {
        checkpoint(control)?;
        let current: Option<(Vec<u8>, i64)> = stored
            .query_row([entry_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(sqlite_error)?;
        if let (Some((bytes, _)), Some(entry)) = (&current, entries.get(&entry_id))
            && bytes.as_slice() != *entry
        {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "lexical artifact row dictionary identifier collided".to_owned(),
            ));
        }
        let references = current.as_ref().map_or(0, |(_, references)| *references) + delta;
        match (current, references) {
            (_, references) if references < 0 => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical dictionary references went negative".to_owned(),
                ));
            }
            (Some(_), 0) => {
                transaction
                    .execute("DELETE FROM row_dictionary WHERE entry_id = ?1", [entry_id])
                    .map_err(sqlite_error)?;
            }
            (Some(_), references) => {
                transaction
                    .execute(
                        "UPDATE row_dictionary SET page_references = ?2 WHERE entry_id = ?1",
                        params![entry_id, references],
                    )
                    .map_err(sqlite_error)?;
            }
            (None, 0) => {}
            (None, references) => {
                let entry = entries.get(&entry_id).ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "carried lexical dictionary entry has no bytes".to_owned(),
                    )
                })?;
                transaction
                    .execute(
                        "INSERT INTO row_dictionary(entry_id, entry, page_references) VALUES (?1, ?2, ?3)",
                        params![entry_id, entry, references],
                    )
                    .map_err(sqlite_error)?;
            }
        }
    }
    Ok(())
}

/// Every dictionary entry the parent's rows in `[first, end)` name.
fn parent_page_dictionary(
    transaction: &Transaction<'_>,
    generation: &CodeGenerationId,
    first: u64,
    end: u64,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<BTreeSet<i64>, CodeLexicalArtifactErrorV1> {
    let dictionary = RecordingRowDictionaryV1 {
        inner: ConnectionRowDictionaryV1::new(transaction),
        referenced: RefCell::new(BTreeSet::new()),
    };
    let mut statement = transaction
        .prepare_cached(
            "SELECT first_document, payload FROM row_blocks WHERE first_document >= ?1 AND first_document < ?2 ORDER BY first_document",
        )
        .map_err(sqlite_error)?;
    let blocks = statement
        .query_map(
            params![
                i64::try_from(first).map_err(contract_number)?,
                i64::try_from(end).map_err(contract_number)?
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    for (first_document, payload) in blocks {
        checkpoint(control)?;
        for stored in decode_row_block(first_document, &payload)? {
            decode_artifact_row(
                generation,
                &stored.chunk_id,
                &stored.row,
                &stored.text,
                &dictionary,
            )?;
        }
    }
    Ok(dictionary.referenced.into_inner())
}

/// Move rowid keys through negative values so no shifted key ever meets one
/// still waiting to move.
fn shift_rowid_keys(
    transaction: &Transaction<'_>,
    table: &str,
    column: &str,
    shift: &ShiftV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let Some(first) = shift.first_affected().filter(|_| shift.shifts()) else {
        return Ok(());
    };
    transaction
        .execute(
            &format!(
                "UPDATE {table} SET {column} = -({image}) - 1 WHERE {column} >= {first}",
                image = shift.sql_image(column)
            ),
            [],
        )
        .map_err(sqlite_error)?;
    transaction
        .execute(
            &format!("UPDATE {table} SET {column} = -{column} - 1 WHERE {column} < 0"),
            [],
        )
        .map_err(sqlite_error)?;
    Ok(())
}

fn carry_row_blocks(
    transaction: &Transaction<'_>,
    documents: &ShiftV1,
    new_pages: &[&PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute(
            &format!(
                "DELETE FROM row_blocks WHERE {}",
                documents.sql_replaced("first_document")
            ),
            [],
        )
        .map_err(sqlite_error)?;
    shift_rowid_keys(transaction, "row_blocks", "first_document", documents)?;
    let mut insert = transaction
        .prepare_cached("INSERT INTO row_blocks(first_document, payload) VALUES (?1, ?2)")
        .map_err(sqlite_error)?;
    for page in new_pages {
        for (first_document, payload) in &page.row_blocks {
            checkpoint(control)?;
            insert
                .execute(params![first_document, payload])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

fn carry_row_chunks(
    transaction: &Transaction<'_>,
    documents: &ShiftV1,
    new_pages: &[&PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    transaction
        .execute(
            &format!(
                "DELETE FROM row_chunks WHERE {}",
                documents.sql_replaced("document_id")
            ),
            [],
        )
        .map_err(sqlite_error)?;
    if let Some(first) = documents.first_affected().filter(|_| documents.shifts()) {
        transaction
            .execute(
                &format!(
                    "UPDATE row_chunks SET document_id = {} WHERE document_id >= {first}",
                    documents.sql_image("document_id")
                ),
                [],
            )
            .map_err(sqlite_error)?;
    }
    let mut insert = transaction
        .prepare_cached("INSERT INTO row_chunks(chunk_id, document_id) VALUES (?1, ?2)")
        .map_err(sqlite_error)?;
    for page in new_pages {
        for document in &page.documents {
            checkpoint(control)?;
            insert
                .execute(params![
                    stored_chunk_key(&document.chunk_id),
                    document.document_id
                ])
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

/// `field_totals` holds each field's net change; a field nothing indexes
/// any more has no row, as in a cold build.
fn apply_field_totals(
    transaction: &Transaction<'_>,
    field_totals: &BTreeMap<i64, i64>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    for (field, delta) in field_totals {
        if *delta == 0 {
            continue;
        }
        let current: i64 = transaction
            .query_row(
                "SELECT total_length FROM field_stats WHERE field = ?1",
                [field],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .unwrap_or(0);
        let total = current + delta;
        if total < 0 {
            return Err(CodeLexicalArtifactErrorV1::Corrupt(
                "carried lexical field total went negative".to_owned(),
            ));
        }
        if total == 0 {
            transaction
                .execute("DELETE FROM field_stats WHERE field = ?1", [field])
                .map_err(sqlite_error)?;
        } else {
            transaction
                .execute(
                    "INSERT INTO field_stats(field, total_length) VALUES (?1, ?2) ON CONFLICT(field) DO UPDATE SET total_length = excluded.total_length",
                    params![field, total],
                )
                .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

fn write_page_rows(
    transaction: &Transaction<'_>,
    parent_pages: &[PageRowV1],
    changed: &[ChangedFileV1],
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let mut rows = Vec::with_capacity(parent_pages.len());
    let mut next_parent = 0usize;
    for file in changed {
        rows.extend_from_slice(&parent_pages[next_parent..file.old_pages.0]);
        for page in &file.pages {
            rows.push(PageRowV1::of_prepared(page)?);
        }
        next_parent = file.old_pages.1;
    }
    rows.extend_from_slice(&parent_pages[next_parent..]);
    transaction
        .execute("DELETE FROM source_pages", [])
        .map_err(sqlite_error)?;
    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO source_pages(page_ordinal, file_ordinal, chunk_count, payload_bytes, import_count, import_payload_bytes, import_digest, clone_body_count, ngram_digest, base_sections_receipt) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .map_err(sqlite_error)?;
    for (ordinal, row) in rows.iter().enumerate() {
        insert
            .execute(params![
                i64::try_from(ordinal).map_err(contract_number)?,
                row.file_ordinal,
                row.chunk_count,
                row.payload_bytes,
                row.import_count,
                row.import_payload_bytes,
                row.import_digest,
                row.clone_body_count,
                row.ngram_digest,
                row.base_sections_receipt,
            ])
            .map_err(sqlite_error)?;
    }
    Ok(rows.len() as u64)
}
