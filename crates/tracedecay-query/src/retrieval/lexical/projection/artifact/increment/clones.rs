//! Carry the parent's clone rows: replace the changed files' occurrences in
//! place and renumber payloads by first use, as a cold build's insert order
//! numbers them.

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Transaction, params};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;

use super::super::builder::stored_digest_key;
use super::super::clone_codec::digest_key;
use super::super::format::{
    contract_number, decode_fingerprint_postings, encode_fingerprint_postings,
};
use super::super::prepared::PreparedCloneBodyV1;
use super::super::row_codec::stored_symbol_key;
use super::super::{CodeLexicalArtifactErrorV1, checkpoint, sqlite_error};
use super::postings::{scan_window, transform_window};
use super::{ChangedFileV1, ShiftV1, shift_rowid_keys, shifted};

/// One successor clone body with the occurrence ordinal it takes.
struct CarriedCloneV1<'a> {
    ordinal: i64,
    body: &'a PreparedCloneBodyV1,
}

/// Replace the changed files' clone occurrences in place, then renumber the
/// payloads by first use over the whole occurrence sequence, which is how a
/// cold build's insert order numbers them.
pub(super) fn carry_clone_rows(
    transaction: &Transaction<'_>,
    clones: &ShiftV1,
    changed: &[ChangedFileV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let mut bodies = Vec::new();
    for (index, file) in changed.iter().enumerate() {
        let first = clone_range_image(clones, index, file)?;
        for (offset, body) in file
            .pages
            .iter()
            .flat_map(|page| &page.clone_bodies)
            .enumerate()
        {
            bodies.push(CarriedCloneV1 {
                ordinal: i64::try_from(first + offset as u64).map_err(contract_number)?,
                body,
            });
        }
    }
    let replaced = clones.sql_replaced("occurrence_ordinal");
    transaction
        .execute(
            &format!("DELETE FROM clone_exact_postings WHERE {replaced}"),
            [],
        )
        .map_err(sqlite_error)?;
    if let Some(first) = clones.first_affected().filter(|_| clones.shifts()) {
        // The posting's ordinal is part of its key, so shifted rows are
        // rewritten through a temporary copy.
        transaction
            .execute_batch(&format!(
                "CREATE TEMP TABLE carried_clone_exact AS SELECT class, normalization_revision, digest, {image} AS occurrence_ordinal FROM clone_exact_postings WHERE occurrence_ordinal >= {first};
                 DELETE FROM clone_exact_postings WHERE occurrence_ordinal >= {first};
                 INSERT INTO clone_exact_postings(class, normalization_revision, digest, occurrence_ordinal) SELECT class, normalization_revision, digest, occurrence_ordinal FROM temp.carried_clone_exact;
                 DROP TABLE temp.carried_clone_exact;",
                image = clones.sql_image("occurrence_ordinal")
            ))
            .map_err(sqlite_error)?;
    }
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_exact_postings(class, normalization_revision, digest, occurrence_ordinal) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(sqlite_error)?;
        for carried in &bodies {
            for key in &carried.body.exact_keys {
                checkpoint(control)?;
                insert
                    .execute(params![
                        i64::from(key.class as u8),
                        i64::from(key.normalization_revision),
                        digest_key(&key.digest)?.as_slice(),
                        carried.ordinal
                    ])
                    .map_err(sqlite_error)?;
            }
        }
    }
    carry_fingerprint_postings(transaction, clones, &bodies, control)?;
    transaction
        .execute(
            &format!(
                "DELETE FROM clone_occurrences WHERE {}",
                clones.sql_replaced("ordinal")
            ),
            [],
        )
        .map_err(sqlite_error)?;
    shift_rowid_keys(transaction, "clone_occurrences", "ordinal", clones)?;
    let mut parent_payloads = BTreeMap::<i64, Vec<u8>>::new();
    {
        let mut statement = transaction
            .prepare("SELECT ordinal, payload_digest FROM clone_body_payloads ORDER BY ordinal")
            .map_err(sqlite_error)?;
        let mut rows = statement.query([]).map_err(sqlite_error)?;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            parent_payloads.insert(
                row.get(0).map_err(sqlite_error)?,
                row.get(1).map_err(sqlite_error)?,
            );
        }
    }
    let mut successor_payloads = BTreeMap::<i64, ([u8; 32], &[u8])>::new();
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_occurrences(ordinal, symbol_key, payload_ordinal, path, body_start, body_end, eligibility) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)",
            )
            .map_err(sqlite_error)?;
        for carried in &bodies {
            checkpoint(control)?;
            let body = carried.body;
            insert
                .execute(params![
                    carried.ordinal,
                    stored_symbol_key(&body.symbol_occurrence_id),
                    body.path,
                    i64::try_from(body.body_start).map_err(contract_number)?,
                    i64::try_from(body.body_end).map_err(contract_number)?,
                    body.eligibility,
                ])
                .map_err(sqlite_error)?;
            successor_payloads.insert(
                carried.ordinal,
                (
                    stored_digest_key(&body.payload_digest)?,
                    body.payload.as_slice(),
                ),
            );
        }
    }
    renumber_clone_payloads(transaction, &parent_payloads, &successor_payloads, control)
}

/// Where a changed file's first successor occurrence lands: its parent
/// start moved by the growth of the changed files before it. `index` pairs
/// the file with its replaced range, since two files without clone bodies
/// can replace the same empty range.
fn clone_range_image(
    clones: &ShiftV1,
    index: usize,
    file: &ChangedFileV1,
) -> Result<u64, CodeLexicalArtifactErrorV1> {
    let start = file.old_clones.0 + 1;
    if clones.replaced.get(index) != Some(&(start, file.old_clones.1 + 1)) {
        return Err(CodeLexicalArtifactErrorV1::Corrupt(
            "carried clone range is missing".to_owned(),
        ));
    }
    let growth_before = index
        .checked_sub(1)
        .map_or(0, |previous| clones.growth[previous]);
    shifted(start, growth_before)
}

fn carry_fingerprint_postings(
    transaction: &Transaction<'_>,
    clones: &ShiftV1,
    bodies: &[CarriedCloneV1<'_>],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    type KeyV1 = (String, i64, i64, i64);
    let mut additions = BTreeMap::<KeyV1, Vec<(u32, u32)>>::new();
    for carried in bodies {
        let Some(stream) = &carried.body.fingerprint_stream else {
            continue;
        };
        let ordinal = u32::try_from(carried.ordinal).map_err(contract_number)?;
        for position in &stream.positions {
            additions
                .entry((
                    stream.language.clone(),
                    i64::from(stream.class as u8),
                    i64::from(stream.normalization_revision),
                    i64::try_from(position.fingerprint).map_err(contract_number)?,
                ))
                .or_default()
                .push((ordinal, position.token_position));
        }
    }
    for postings in additions.values_mut() {
        postings.sort_unstable();
    }
    let mut last: Option<KeyV1> = None;
    loop {
        checkpoint(control)?;
        let window = scan_window(
            transaction,
            "SELECT language, class, normalization_revision, fingerprint, postings FROM clone_fingerprint_postings ORDER BY language, class, normalization_revision, fingerprint LIMIT ?1",
            "SELECT language, class, normalization_revision, fingerprint, postings FROM clone_fingerprint_postings WHERE (language, class, normalization_revision, fingerprint) > (?1, ?2, ?3, ?4) ORDER BY language, class, normalization_revision, fingerprint LIMIT ?5",
            &last,
            |(language, class, revision, fingerprint)| {
                vec![
                    Value::Text(language.clone()),
                    Value::Integer(*class),
                    Value::Integer(*revision),
                    Value::Integer(*fingerprint),
                ]
            },
            |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ),
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )?;
        let Some((final_key, _)) = window.last() else {
            break;
        };
        last = Some(final_key.clone());
        let jobs: Vec<_> = window
            .into_iter()
            .map(|(key, stored)| {
                let key_additions = additions.remove(&key).unwrap_or_default();
                (key, stored, key_additions)
            })
            .collect();
        let carried = transform_window(&jobs, |(_, stored, key_additions)| {
            carry_fingerprint_row(stored, clones, key_additions)?
                .map(|postings| {
                    Ok::<_, CodeLexicalArtifactErrorV1>((
                        i64::try_from(postings.len()).map_err(contract_number)?,
                        (!postings.is_empty())
                            .then(|| encode_fingerprint_postings(&postings))
                            .transpose()?,
                    ))
                })
                .transpose()
        })?;
        for ((key, _, _), sealed) in jobs.iter().zip(carried) {
            let Some((posting_count, sealed)) = sealed else {
                continue;
            };
            let Some(sealed) = sealed else {
                transaction
                    .execute(
                        "DELETE FROM clone_fingerprint_postings WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
                        params![key.0, key.1, key.2, key.3],
                    )
                    .map_err(sqlite_error)?;
                continue;
            };
            transaction
                .execute(
                    "UPDATE clone_fingerprint_postings SET posting_count = ?5, postings = ?6 WHERE language = ?1 AND class = ?2 AND normalization_revision = ?3 AND fingerprint = ?4",
                    params![key.0, key.1, key.2, key.3, posting_count, sealed],
                )
                .map_err(sqlite_error)?;
        }
    }
    for ((language, class, revision, fingerprint), postings) in additions {
        checkpoint(control)?;
        transaction
            .execute(
                "INSERT INTO clone_fingerprint_postings(language, class, normalization_revision, fingerprint, posting_count, postings) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    language,
                    class,
                    revision,
                    fingerprint,
                    i64::try_from(postings.len()).map_err(contract_number)?,
                    encode_fingerprint_postings(&postings)?
                ],
            )
            .map_err(sqlite_error)?;
    }
    Ok(())
}

/// One fingerprint's postings after the carry, or `None` when unchanged.
fn carry_fingerprint_row(
    stored: &[u8],
    clones: &ShiftV1,
    additions: &[(u32, u32)],
) -> Result<Option<Vec<(u32, u32)>>, CodeLexicalArtifactErrorV1> {
    let mut changed = !additions.is_empty();
    let mut postings = Vec::new();
    for (ordinal, position) in decode_fingerprint_postings(stored)? {
        match clones.map_u32(ordinal)? {
            Some(mapped) => {
                changed |= mapped != ordinal;
                postings.push((mapped, position));
            }
            None => changed = true,
        }
    }
    if !changed {
        return Ok(None);
    }
    postings.extend_from_slice(additions);
    postings.sort_unstable();
    Ok(Some(postings))
}

/// Number payloads by first use over the occurrence sequence, retire the
/// ones no occurrence uses, and point every occurrence at its number.
fn renumber_clone_payloads(
    transaction: &Transaction<'_>,
    parent_payloads: &BTreeMap<i64, Vec<u8>>,
    successor_payloads: &BTreeMap<i64, ([u8; 32], &[u8])>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let parent_by_digest: BTreeMap<&[u8], i64> = parent_payloads
        .iter()
        .map(|(ordinal, digest)| (digest.as_slice(), *ordinal))
        .collect();
    let occurrences: Vec<(i64, i64)> = {
        let mut statement = transaction
            .prepare("SELECT ordinal, payload_ordinal FROM clone_occurrences ORDER BY ordinal")
            .map_err(sqlite_error)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?
    };
    let mut numbered = BTreeMap::<Vec<u8>, i64>::new();
    let mut fresh = Vec::<(i64, [u8; 32], &[u8])>::new();
    let mut repointed = Vec::new();
    for (ordinal, payload_ordinal) in occurrences {
        checkpoint(control)?;
        let digest: Vec<u8> = match successor_payloads.get(&ordinal) {
            Some((digest, _)) => digest.to_vec(),
            None => parent_payloads
                .get(&payload_ordinal)
                .cloned()
                .ok_or_else(|| {
                    CodeLexicalArtifactErrorV1::Corrupt(
                        "carried clone occurrence names a missing payload".to_owned(),
                    )
                })?,
        };
        let next = numbered.len() as i64 + 1;
        let number = *numbered.entry(digest.clone()).or_insert_with(|| {
            if !parent_by_digest.contains_key(digest.as_slice())
                && let Some((key, payload)) = successor_payloads.get(&ordinal)
            {
                fresh.push((next, *key, payload));
            }
            next
        });
        if number != payload_ordinal {
            repointed.push((ordinal, number));
        }
    }
    // A successor body whose payload the parent already stores must match it.
    for (key, payload) in successor_payloads.values() {
        if let Some(ordinal) = parent_by_digest.get(key.as_slice()) {
            let stored: Vec<u8> = transaction
                .query_row(
                    "SELECT payload FROM clone_body_payloads WHERE ordinal = ?1",
                    [ordinal],
                    |row| row.get(0),
                )
                .map_err(sqlite_error)?;
            if stored.as_slice() != *payload {
                return Err(CodeLexicalArtifactErrorV1::Corrupt(
                    "clone payload digest names different bytes".to_owned(),
                ));
            }
        }
    }
    for (ordinal, digest) in parent_payloads {
        match numbered.get(digest) {
            None => {
                transaction
                    .execute(
                        "DELETE FROM clone_body_payloads WHERE ordinal = ?1",
                        [ordinal],
                    )
                    .map_err(sqlite_error)?;
            }
            Some(number) if number != ordinal => {
                transaction
                    .execute(
                        "UPDATE clone_body_payloads SET ordinal = ?2 WHERE ordinal = ?1",
                        params![ordinal, -number - 1],
                    )
                    .map_err(sqlite_error)?;
            }
            Some(_) => {}
        }
    }
    transaction
        .execute(
            "UPDATE clone_body_payloads SET ordinal = -ordinal - 1 WHERE ordinal < 0",
            [],
        )
        .map_err(sqlite_error)?;
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO clone_body_payloads(ordinal, payload_digest, payload) VALUES (?1, ?2, ?3)",
            )
            .map_err(sqlite_error)?;
        for (number, key, payload) in fresh {
            insert
                .execute(params![number, key.as_slice(), payload])
                .map_err(sqlite_error)?;
        }
    }
    let mut repoint = transaction
        .prepare_cached("UPDATE clone_occurrences SET payload_ordinal = ?2 WHERE ordinal = ?1")
        .map_err(sqlite_error)?;
    for (ordinal, number) in repointed {
        repoint
            .execute(params![ordinal, number])
            .map_err(sqlite_error)?;
    }
    Ok(())
}
