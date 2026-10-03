//! Carry the parent's posting lists: drop the replaced documents, move the
//! carried ones, and merge the successor pages' postings.

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
use rusqlite::types::Value;
use rusqlite::{Transaction, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::super::format::{
    PostingListDecoderV1, PostingListEncoderV1, contract_number, decode_document_set,
    decode_term_lists, encode_term_lists, term_lists_bytes,
};
use super::super::prepared::PreparedCodeLexicalArtifactPageV1;
use super::super::schema::{
    exact_field_code_from_encoded, field_code, intern_exact_terms, stable_exact_term_id,
};
use super::super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_error};
use super::{CARRY_SCAN_ROWS, ShiftV1};
use crate::retrieval::lexical::LexicalFieldV1;

/// Carry one stored list: drop replaced documents (reporting each), move the
/// rest, and merge the successor's postings, which lie wholly inside the
/// replaced ranges. Returns `None` when nothing changed.
pub(super) fn carry_list<I>(
    stored: impl Fn() -> I,
    documents: &ShiftV1,
    additions: &[(u32, u32)],
    removed: &mut dyn FnMut(u32),
) -> Result<Option<Vec<(u32, u32)>>, CodeLexicalArtifactErrorV1>
where
    I: Iterator<Item = Result<(u32, u32), CodeLexicalArtifactErrorV1>>,
{
    // Most lists hold no replaced or moved document; they are read only up
    // to the point past which nothing moves, and never rebuilt.
    if additions.is_empty() && !documents.moves_any(stored())? {
        return Ok(None);
    }
    let mut changed = !additions.is_empty();
    let mut carried = Vec::new();
    for posting in stored() {
        let (document, frequency) = posting?;
        match documents.map_u32(document)? {
            Some(mapped) => {
                changed |= mapped != document;
                carried.push((mapped, frequency));
            }
            None => {
                changed = true;
                removed(frequency);
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    let mut merged = Vec::with_capacity(carried.len() + additions.len());
    let (mut left, mut right) = (
        carried.into_iter().peekable(),
        additions.iter().copied().peekable(),
    );
    loop {
        match (left.peek(), right.peek()) {
            (Some(a), Some(b)) if a.0 < b.0 => merged.extend(left.next()),
            (Some(a), Some(b)) if a.0 == b.0 => {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "carried lexical posting meets a successor posting".to_owned(),
                ));
            }
            (Some(_), Some(_)) | (None, Some(_)) => merged.extend(right.next()),
            (Some(_), None) => merged.extend(left.next()),
            (None, None) => break,
        }
    }
    Ok(Some(merged))
}

pub(super) fn encode_list(
    postings: &[(u32, u32)],
    frequencies: bool,
) -> Result<PostingListEncoderV1, CodeLexicalArtifactErrorV1> {
    let mut encoder = PostingListEncoderV1::new(frequencies);
    for (document, frequency) in postings {
        encoder.push(*document, *frequency)?;
    }
    Ok(encoder)
}

/// Rows one pool task carries under a single CPU permit.
const CARRY_TASK_ROWS: usize = 128;

/// A position as a document-set bound: documents are `u32`, so a position
/// past that range bounds an empty remainder.
fn set_bound(position: u64) -> u32 {
    u32::try_from(position).unwrap_or(u32::MAX)
}

/// Carry one window's rows on the indexing pool. Decoding, moving, and
/// re-encoding a list touches no SQLite state, so only the writes stay on
/// the caller's thread.
pub(super) fn transform_window<I, O>(
    rows: &[I],
    transform: impl Fn(&I) -> Result<O, CodeLexicalArtifactErrorV1> + Sync,
) -> Result<Vec<O>, CodeLexicalArtifactErrorV1>
where
    I: Sync,
    O: Send,
{
    let chunks = tracedecay_code_index::parallelism::install(|| {
        rows.par_chunks(CARRY_TASK_ROWS)
            .map(|chunk| {
                tracedecay_code_index::parallelism::with_background_cpu_permit(|| {
                    chunk.iter().map(&transform).collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .map_err(|error| CodeLexicalArtifactErrorV1::Io(error.to_string()))??;
    Ok(chunks.into_iter().flatten().collect())
}

/// Read one bounded, key-ordered window of a serving table.
pub(super) fn scan_window<K, R>(
    transaction: &Transaction<'_>,
    first: &str,
    after: &str,
    last: &Option<K>,
    bind: impl Fn(&K) -> Vec<Value>,
    read: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<R>,
) -> Result<Vec<R>, CodeLexicalArtifactErrorV1> {
    let limit = Value::Integer(CARRY_SCAN_ROWS as i64);
    let (sql, mut values) = match last {
        None => (first, Vec::new()),
        Some(last) => (after, bind(last)),
    };
    values.push(limit);
    let mut statement = transaction.prepare_cached(sql).map_err(sqlite_error)?;
    statement
        .query_map(rusqlite::params_from_iter(values), read)
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}

pub(super) fn carry_term_postings(
    transaction: &Transaction<'_>,
    documents: &ShiftV1,
    new_pages: &[&PreparedCodeLexicalArtifactPageV1],
    field_totals: &mut BTreeMap<i64, i64>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut additions = BTreeMap::<&str, BTreeMap<i64, Vec<(u32, u32)>>>::new();
    for document in new_pages.iter().flat_map(|page| &page.documents) {
        let document_id = u32::try_from(document.document_id).map_err(contract_number)?;
        for posting in &document.term_postings {
            let frequency = u32::try_from(posting.frequency).map_err(contract_number)?;
            additions
                .entry(posting.term.as_str())
                .or_default()
                .entry(posting.field_code)
                .or_default()
                .push((document_id, frequency));
            *field_totals.entry(posting.field_code).or_default() += posting.frequency;
        }
    }
    let subtoken = field_code(LexicalFieldV1::Subtoken);
    let mut last: Option<String> = None;
    loop {
        checkpoint(control)?;
        let window = scan_window(
            transaction,
            "SELECT term, lists FROM term_postings ORDER BY term LIMIT ?1",
            "SELECT term, lists FROM term_postings WHERE term > ?1 ORDER BY term LIMIT ?2",
            &last,
            |term| vec![Value::Text(term.clone())],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let Some((final_term, _)) = window.last() else {
            break;
        };
        last = Some(final_term.clone());
        let jobs: Vec<_> = window
            .into_iter()
            .map(|(term, stored)| {
                let term_additions = additions.remove(term.as_str()).unwrap_or_default();
                (term, stored, term_additions)
            })
            .collect();
        let carried = transform_window(&jobs, |(_, stored, term_additions)| {
            carry_term_row(stored, documents, term_additions)
        })?;
        for ((term, _, _), (sealed, removed)) in jobs.iter().zip(carried) {
            for (field, frequency) in removed {
                *field_totals.entry(field).or_default() -= frequency;
            }
            let Some(sealed) = sealed else {
                continue;
            };
            if sealed.is_empty() {
                transaction
                    .execute("DELETE FROM term_postings WHERE term = ?1", [term])
                    .map_err(sqlite_error)?;
            } else {
                let in_fuzzy = sealed.iter().any(|(field, _, _)| *field != subtoken);
                transaction
                    .execute(
                        "UPDATE term_postings SET in_fuzzy = ?2, lists = ?3 WHERE term = ?1",
                        params![term, in_fuzzy, encode_term_lists(&sealed)?],
                    )
                    .map_err(sqlite_error)?;
            }
        }
    }
    for (term, fields) in additions {
        checkpoint(control)?;
        let sealed = fields
            .into_iter()
            .map(|(field, postings)| sealed_list(field, &postings))
            .collect::<Result<Vec<_>, _>>()?;
        let in_fuzzy = sealed.iter().any(|(field, _, _)| *field != subtoken);
        transaction
            .execute(
                "INSERT INTO term_postings(term, in_fuzzy, lists) VALUES (?1, ?2, ?3)",
                params![term, in_fuzzy, encode_term_lists(&sealed)?],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

/// One term's sealed field lists after the carry, or `None` when nothing
/// about the term changed; an empty result means no field still lists it.
/// Also returns, per field, the summed frequency of the postings it dropped.
fn carry_term_row(
    stored: &[u8],
    documents: &ShiftV1,
    additions: &BTreeMap<i64, Vec<(u32, u32)>>,
) -> Result<CarriedTermRowV1, CodeLexicalArtifactErrorV1> {
    let bytes = term_lists_bytes(stored)?;
    let mut changed = !additions.is_empty();
    let mut sealed = Vec::new();
    let mut dropped = Vec::new();
    let mut fresh: Vec<i64> = additions.keys().copied().collect();
    for (field, document_frequency, list) in decode_term_lists(&bytes)? {
        fresh.retain(|candidate| *candidate != field);
        let field_additions = additions.get(&field).map_or(&[][..], Vec::as_slice);
        let mut removed_frequency = 0i64;
        let mut removed = |frequency: u32| removed_frequency += i64::from(frequency);
        let carried = carry_list(
            || PostingListDecoderV1::new(list, true),
            documents,
            field_additions,
            &mut removed,
        )?;
        if removed_frequency != 0 {
            dropped.push((field, removed_frequency));
        }
        match carried {
            None => sealed.push((field, document_frequency, list.to_vec())),
            Some(postings) => {
                changed = true;
                if !postings.is_empty() {
                    sealed.push(sealed_list(field, &postings)?);
                }
            }
        }
    }
    if !changed {
        return Ok((None, dropped));
    }
    for field in fresh {
        sealed.push(sealed_list(field, &additions[&field])?);
    }
    sealed.sort_by_key(|(field, _, _)| *field);
    Ok((Some(sealed), dropped))
}

/// A carried term's sealed field lists (`None` when unchanged) and the
/// frequency it dropped per field.
type CarriedTermRowV1 = (Option<Vec<(i64, u64, Vec<u8>)>>, Vec<(i64, i64)>);

fn sealed_list(
    field: i64,
    postings: &[(u32, u32)],
) -> Result<(i64, u64, Vec<u8>), CodeLexicalArtifactErrorV1> {
    let encoder = encode_list(postings, true)?;
    Ok((field, encoder.len(), encoder.finish()?))
}

pub(super) fn carry_exact_postings(
    transaction: &Transaction<'_>,
    documents: &ShiftV1,
    new_pages: &[&PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut additions = BTreeMap::<(i64, i64), Vec<(u32, u32)>>::new();
    let mut vocabulary = BTreeMap::<i64, &[u8]>::new();
    for document in new_pages.iter().flat_map(|page| &page.documents) {
        let document_id = u32::try_from(document.document_id).map_err(contract_number)?;
        for (field, term) in &document.exact_postings {
            let term_id = stable_exact_term_id(term);
            vocabulary.insert(term_id, term.as_slice());
            additions
                .entry((term_id, exact_field_code_from_encoded(field)?))
                .or_default()
                .push((document_id, 1));
        }
    }
    let interned: Vec<(&[u8], i64)> = vocabulary.iter().map(|(id, term)| (*term, *id)).collect();
    intern_exact_terms(transaction, &interned, control)?;
    let mut emptied = BTreeSet::new();
    let mut last: Option<(i64, i64)> = None;
    loop {
        checkpoint(control)?;
        let window = scan_window(
            transaction,
            "SELECT term_id, field, documents FROM exact_postings ORDER BY term_id, field LIMIT ?1",
            "SELECT term_id, field, documents FROM exact_postings WHERE (term_id, field) > (?1, ?2) ORDER BY term_id, field LIMIT ?3",
            &last,
            |(term_id, field)| vec![Value::Integer(*term_id), Value::Integer(*field)],
            |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        let Some((final_key, _)) = window.last() else {
            break;
        };
        last = Some(*final_key);
        let jobs: Vec<_> = window
            .into_iter()
            .map(|(key, stored)| {
                let key_additions = additions.remove(&key).unwrap_or_default();
                (key, stored, key_additions)
            })
            .collect();
        let carried = transform_window(&jobs, |(_, stored, key_additions)| {
            carry_list(
                || PostingListDecoderV1::new(stored, false),
                documents,
                key_additions,
                &mut |_| {},
            )?
            .map(|postings| {
                (!postings.is_empty())
                    .then(|| encode_list(&postings, false)?.finish())
                    .transpose()
            })
            .transpose()
        })?;
        for ((key, _, _), sealed) in jobs.iter().zip(carried) {
            let Some(sealed) = sealed else {
                continue;
            };
            let Some(sealed) = sealed else {
                emptied.insert(key.0);
                transaction
                    .execute(
                        "DELETE FROM exact_postings WHERE term_id = ?1 AND field = ?2",
                        params![key.0, key.1],
                    )
                    .map_err(sqlite_error)?;
                continue;
            };
            transaction
                .execute(
                    "UPDATE exact_postings SET documents = ?3 WHERE term_id = ?1 AND field = ?2",
                    params![key.0, key.1, sealed],
                )
                .map_err(sqlite_error)?;
        }
    }
    for ((term_id, field), postings) in additions {
        checkpoint(control)?;
        transaction
            .execute(
                "INSERT INTO exact_postings(term_id, field, documents) VALUES (?1, ?2, ?3)",
                params![term_id, field, encode_list(&postings, false)?.finish()?],
            )
            .map_err(sqlite_error)?;
    }
    for term_id in emptied {
        transaction
            .execute(
                "DELETE FROM exact_vocabulary WHERE term_id = ?1 AND NOT EXISTS(SELECT 1 FROM exact_postings WHERE term_id = ?1)",
                [term_id],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

pub(super) fn carry_ngram_postings(
    transaction: &Transaction<'_>,
    documents: &ShiftV1,
    new_pages: &[&PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut additions = BTreeMap::<(i64, i64), Vec<(u32, u32)>>::new();
    for page in new_pages {
        let page_first = u32::try_from(page.first_document).map_err(contract_number)?;
        for shard in &page.ngram_shards {
            let entry = additions.entry((shard.kind, shard.ngram)).or_default();
            for posting in PostingListDecoderV1::new(&shard.documents, false) {
                let (local, _) = posting?;
                entry.push((page_first + local, 1));
            }
        }
    }
    let mut last: Option<(i64, i64)> = None;
    loop {
        checkpoint(control)?;
        let window = scan_window(
            transaction,
            "SELECT kind, ngram, documents FROM ngram_postings ORDER BY kind, ngram LIMIT ?1",
            "SELECT kind, ngram, documents FROM ngram_postings WHERE (kind, ngram) > (?1, ?2) ORDER BY kind, ngram LIMIT ?3",
            &last,
            |(kind, ngram)| vec![Value::Integer(*kind), Value::Integer(*ngram)],
            |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        let Some((final_key, _)) = window.last() else {
            break;
        };
        last = Some(*final_key);
        let jobs: Vec<_> = window
            .into_iter()
            .map(|(key, stored)| {
                let key_additions = additions.remove(&key).unwrap_or_default();
                (key, stored, key_additions)
            })
            .collect();
        let carried = transform_window(&jobs, |(_, stored, key_additions)| {
            let parent = decode_document_set(stored)?;
            if key_additions.is_empty()
                && !documents.moves_any_in(|start, end| {
                    parent.range_cardinality(set_bound(start)..set_bound(end)) > 0
                })
            {
                return Ok(None);
            }
            carry_list(
                || parent.iter().map(|document| Ok((document, 1))),
                documents,
                key_additions,
                &mut |_| {},
            )?
            .map(|postings| {
                (!postings.is_empty())
                    .then(|| {
                        let encoder = encode_list(&postings, false)?;
                        let frequency = i64::try_from(encoder.len()).map_err(contract_number)?;
                        Ok((frequency, encoder.finish_document_set()?))
                    })
                    .transpose()
            })
            .transpose()
        })?;
        for ((key, _, _), sealed) in jobs.iter().zip(carried) {
            let Some(sealed) = sealed else {
                continue;
            };
            let Some((frequency, sealed)) = sealed else {
                transaction
                    .execute(
                        "DELETE FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
                        params![key.0, key.1],
                    )
                    .map_err(sqlite_error)?;
                continue;
            };
            transaction
                .execute(
                    "UPDATE ngram_postings SET document_frequency = ?3, documents = ?4 WHERE kind = ?1 AND ngram = ?2",
                    params![key.0, key.1, frequency, sealed],
                )
                .map_err(sqlite_error)?;
        }
    }
    for ((kind, ngram), postings) in additions {
        checkpoint(control)?;
        let encoder = encode_list(&postings, false)?;
        let frequency = i64::try_from(encoder.len()).map_err(contract_number)?;
        transaction
            .execute(
                "INSERT INTO ngram_postings(kind, ngram, document_frequency, documents) VALUES (?1, ?2, ?3, ?4)",
                params![kind, ngram, frequency, encoder.finish_document_set()?],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}
